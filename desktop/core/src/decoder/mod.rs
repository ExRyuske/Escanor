//! Аппаратное декодирование H.264: Media Foundation + D3D11 на Windows, VideoToolbox на macOS.
//! Оба декодера отдают NV12 — тот же формат, что нужен виртуальной камере.

use anyhow::Result;

#[cfg(windows)]
mod mf;
#[cfg(target_os = "macos")]
mod videotoolbox;

/// Декодированный кадр NV12: плоскость яркости и чередующиеся U/V на половинном разрешении.
pub struct Nv12Frame<'a> {
    pub width: usize,
    pub height: usize,
    pub y: &'a [u8],
    pub y_stride: usize,
    pub uv: &'a [u8],
    pub uv_stride: usize,
}

pub trait Decoder {
    /// Подаёт один кадр в формате Annex B; `on_frame` вызывается для каждого готового кадра.
    fn decode(&mut self, data: &[u8], pts_us: i64, on_frame: &mut dyn FnMut(&Nv12Frame)) -> Result<()>;
    /// Название для интерфейса, например «VideoToolbox (аппаратный)».
    fn description(&self) -> String;
}

/// Создаёт декодер; вызывать в потоке, где он будет работать (COM на Windows).
pub fn create() -> Result<Box<dyn Decoder>> {
    #[cfg(windows)]
    return Ok(Box::new(mf::MfDecoder::new()?));
    #[cfg(target_os = "macos")]
    return Ok(Box::new(videotoolbox::VtDecoder::new()));
    #[allow(unreachable_code)]
    Err(anyhow::anyhow!("аппаратный декодер для этой ОС не реализован"))
}

/// NAL-блоки из потока Annex B, без стартовых кодов.
pub fn nal_units(data: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut pos = find_start(data, 0);
    std::iter::from_fn(move || {
        let (_, begin) = pos?;
        let next = find_start(data, begin);
        let end = match next {
            // Ведущий ноль четырёхбайтового стартового кода относится к следующему коду.
            Some((code, _)) if code > begin && data[code - 1] == 0 => code - 1,
            Some((code, _)) => code,
            None => data.len(),
        };
        pos = next;
        Some(&data[begin..end])
    })
    .filter(|nal| !nal.is_empty())
}

/// Позиция следующего стартового кода `00 00 01` и начало NAL-блока после него.
fn find_start(data: &[u8], from: usize) -> Option<(usize, usize)> {
    data.get(from..)?.windows(3).position(|w| w == [0, 0, 1]).map(|i| (from + i, from + i + 3))
}

impl Nv12Frame<'_> {
    /// Копирует кадр в NV12 без отступов строк.
    pub fn write_nv12(&self, dst: &mut [u8]) {
        let (w, h) = (self.width, self.height);
        let (y_dst, uv_dst) = dst.split_at_mut(w * h);
        for row in 0..h {
            y_dst[row * w..][..w].copy_from_slice(&self.y[row * self.y_stride..][..w]);
        }
        for row in 0..h / 2 {
            uv_dst[row * w..][..w].copy_from_slice(&self.uv[row * self.uv_stride..][..w]);
        }
    }

    /// Уменьшенная RGBA-копия для превью. Кодировщик телефона выдаёт BT.709 limited range.
    pub fn to_preview(&self, max_width: usize) -> crate::video::PreviewFrame {
        let step = self.width.div_ceil(max_width).max(1);
        let (pw, ph) = (self.width / step, self.height / step);
        let mut rgba = vec![0u8; pw * ph * 4];
        for py in 0..ph {
            let sy = py * step;
            let y_row = &self.y[sy * self.y_stride..];
            let uv_row = &self.uv[(sy / 2) * self.uv_stride..];
            let out = &mut rgba[py * pw * 4..][..pw * 4];
            for (px, pixel) in out.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                let sx = px * step;
                let c = (y_row[sx] as i32 - 16) * 298;
                let d = uv_row[sx & !1] as i32 - 128;
                let e = uv_row[(sx & !1) + 1] as i32 - 128;
                pixel[0] = ((c + 459 * e + 128) >> 8).clamp(0, 255) as u8;
                pixel[1] = ((c - 55 * d - 136 * e + 128) >> 8).clamp(0, 255) as u8;
                pixel[2] = ((c + 541 * d + 128) >> 8).clamp(0, 255) as u8;
                pixel[3] = 255;
            }
        }
        crate::video::PreviewFrame { width: pw as u32, height: ph as u32, rgba }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_annex_b() {
        let data = [0, 0, 0, 1, 0x67, 1, 2, 0, 0, 1, 0x68, 3, 0, 0, 0, 1, 0x65, 4, 5];
        let units: Vec<&[u8]> = nal_units(&data).collect();
        assert_eq!(units, [&[0x67, 1, 2][..], &[0x68, 3][..], &[0x65, 4, 5][..]]);
    }

    #[test]
    fn nv12_drops_padding() {
        // 4×2 со строками по 6 байт.
        let y = [1, 2, 3, 4, 0, 0, 5, 6, 7, 8, 0, 0];
        let uv = [10, 20, 11, 21, 0, 0];
        let frame = Nv12Frame { width: 4, height: 2, y: &y, y_stride: 6, uv: &uv, uv_stride: 6 };
        let mut out = vec![0; 12];
        frame.write_nv12(&mut out);
        assert_eq!(out, [1, 2, 3, 4, 5, 6, 7, 8, 10, 20, 11, 21]);
    }

    #[test]
    fn preview_colors() {
        // Белый и чёрный в limited range при нейтральном цвете.
        let y = [235, 235, 16, 16];
        let uv = [128, 128];
        let frame = Nv12Frame { width: 2, height: 2, y: &y, y_stride: 2, uv: &uv, uv_stride: 2 };
        let p = frame.to_preview(960);
        assert_eq!(&p.rgba[0..4], &[255, 255, 255, 255]);
        assert_eq!(&p.rgba[8..12], &[0, 0, 0, 255]);
    }
}
