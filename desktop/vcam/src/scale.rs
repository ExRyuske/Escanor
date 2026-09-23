//! Масштабирование NV12 под разрешение, которое выбрало приложение-потребитель.
//! Пропорции сохраняются (чёрные поля при несовпадении), фильтр — билинейный.
//! Не зависит от Windows, поэтому тестируется на любой ОС.

const BLACK_Y: u8 = 16;
const NEUTRAL_UV: u8 = 128;
/// Заглушка «нет сигнала» — тёмно-серый кадр, чтобы было видно, что камера работает.
const PLACEHOLDER_Y: u8 = 40;

/// Буфер назначения NV12: плоскость Y из `height` строк по `pitch` байт, сразу за ней — UV.
pub struct Nv12Target<'a> {
    pub data: &'a mut [u8],
    pub pitch: usize,
    pub width: usize,
    pub height: usize,
}

impl Nv12Target<'_> {
    fn split(&mut self) -> (&mut [u8], &mut [u8]) {
        self.data.split_at_mut(self.pitch * self.height)
    }

    pub fn fill(&mut self, y: u8) {
        let (w, h, pitch) = (self.width, self.height, self.pitch);
        let (y_plane, uv_plane) = self.split();
        for row in 0..h {
            y_plane[row * pitch..][..w].fill(y);
        }
        for row in 0..h / 2 {
            uv_plane[row * pitch..][..w].fill(NEUTRAL_UV);
        }
    }

    pub fn fill_placeholder(&mut self) {
        self.fill(PLACEHOLDER_Y);
    }
}

/// Прямоугольник вписанного изображения (чётные координаты — из-за субдискретизации цвета).
fn fit(sw: usize, sh: usize, dw: usize, dh: usize) -> (usize, usize, usize, usize) {
    let (rw, rh) = if sw * dh > dw * sh { (dw, sh * dw / sw) } else { (sw * dh / sh, dh) };
    let (rw, rh) = (rw.max(2) & !1, rh.max(2) & !1);
    (((dw - rw) / 2) & !1, ((dh - rh) / 2) & !1, rw, rh)
}

/// Для каждой выходной координаты: индексы соседей и вес правого (0..=256).
fn taps(src: usize, dst: usize) -> Vec<(usize, usize, u32)> {
    (0..dst)
        .map(|d| {
            let pos = ((d as f64 + 0.5) * src as f64 / dst as f64 - 0.5).clamp(0.0, (src - 1) as f64);
            let i = pos as usize;
            let frac = ((pos - i as f64) * 256.0).round() as u32;
            (i, (i + 1).min(src - 1), frac.min(256))
        })
        .collect()
}

#[inline]
fn lerp(a: u8, b: u8, w: u32) -> u32 {
    a as u32 * (256 - w) + b as u32 * w
}

/// Вписывает кадр NV12 `src` (`sw`×`sh`, без отступов строк) в `dst`.
pub fn scale_nv12(src: &[u8], sw: usize, sh: usize, dst: &mut Nv12Target) {
    let (dw, dh, pitch) = (dst.width, dst.height, dst.pitch);
    let (src_y, src_uv) = src.split_at(sw * sh);

    if sw == dw && sh == dh {
        let (y_plane, uv_plane) = dst.split();
        for row in 0..dh {
            y_plane[row * pitch..][..dw].copy_from_slice(&src_y[row * sw..][..sw]);
        }
        for row in 0..dh / 2 {
            uv_plane[row * pitch..][..dw].copy_from_slice(&src_uv[row * sw..][..sw]);
        }
        return;
    }

    let (x0, y0, rw, rh) = fit(sw, sh, dw, dh);
    if rw != dw || rh != dh {
        dst.fill(BLACK_Y);
    }
    let (y_plane, uv_plane) = dst.split();

    // Яркость.
    let xs = taps(sw, rw);
    for (row, (sy, sy1, fy)) in taps(sh, rh).into_iter().enumerate() {
        let top = &src_y[sy * sw..][..sw];
        let bottom = &src_y[sy1 * sw..][..sw];
        let out = &mut y_plane[(y0 + row) * pitch + x0..][..rw];
        for (o, &(sx, sx1, fx)) in out.iter_mut().zip(&xs) {
            let t = lerp(top[sx], top[sx1], fx);
            let b = lerp(bottom[sx], bottom[sx1], fx);
            *o = ((t * (256 - fy) + b * fy + (1 << 15)) >> 16) as u8;
        }
    }

    // Цвет: пары U/V на половинном разрешении.
    let (scw, sch, rcw, rch) = (sw / 2, sh / 2, rw / 2, rh / 2);
    let xs = taps(scw, rcw);
    for (row, (sy, sy1, fy)) in taps(sch, rch).into_iter().enumerate() {
        let top = &src_uv[sy * sw..][..scw * 2];
        let bottom = &src_uv[sy1 * sw..][..scw * 2];
        let out = &mut uv_plane[(y0 / 2 + row) * pitch + x0..][..rcw * 2];
        for (pair, &(sx, sx1, fx)) in out.as_chunks_mut::<2>().0.iter_mut().zip(&xs) {
            for c in 0..2 {
                let t = lerp(top[sx * 2 + c], top[sx1 * 2 + c], fx);
                let b = lerp(bottom[sx * 2 + c], bottom[sx1 * 2 + c], fx);
                pair[c] = ((t * (256 - fy) + b * fy + (1 << 15)) >> 16) as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(w: usize, h: usize, y: u8, uv: u8) -> Vec<u8> {
        let mut f = vec![y; w * h];
        f.extend(std::iter::repeat_n(uv, w * h / 2));
        f
    }

    #[test]
    fn same_size_copies_rows_with_pitch() {
        let src: Vec<u8> = (0..24).collect(); // 4×4: 16 байт Y + 8 байт UV
        let mut buf = vec![0xAA; 6 * 6];
        scale_nv12(&src, 4, 4, &mut Nv12Target { data: &mut buf, pitch: 6, width: 4, height: 4 });
        assert_eq!(&buf[0..4], &[0, 1, 2, 3]);
        assert_eq!(&buf[4..6], &[0xAA, 0xAA], "отступ строки не трогаем");
        assert_eq!(&buf[18..22], &[12, 13, 14, 15]);
        assert_eq!(&buf[24..28], &[16, 17, 18, 19]);
        assert_eq!(&buf[30..34], &[20, 21, 22, 23]);
    }

    #[test]
    fn downscale_keeps_flat_color() {
        let src = frame(1920, 1080, 200, 90);
        let mut buf = vec![0; 1280 * 720 * 3 / 2];
        scale_nv12(&src, 1920, 1080, &mut Nv12Target { data: &mut buf, pitch: 1280, width: 1280, height: 720 });
        assert!(buf[..1280 * 720].iter().all(|&v| v == 200));
        assert!(buf[1280 * 720..].iter().all(|&v| v == 90));
    }

    #[test]
    fn letterboxes_4_3_into_16_9() {
        let src = frame(640, 480, 200, 90);
        let mut buf = vec![0; 1280 * 720 * 3 / 2];
        scale_nv12(&src, 640, 480, &mut Nv12Target { data: &mut buf, pitch: 1280, width: 1280, height: 720 });
        // Вписанная ширина 960, поля по 160 слева и справа.
        let row = &buf[360 * 1280..][..1280];
        assert!(row[..160].iter().all(|&v| v == BLACK_Y));
        assert!(row[160..1120].iter().all(|&v| v == 200));
        assert!(row[1120..].iter().all(|&v| v == BLACK_Y));
        let uv_row = &buf[1280 * 720 + 100 * 1280..][..1280];
        assert!(uv_row[..160].iter().all(|&v| v == NEUTRAL_UV));
        assert!(uv_row[160..1120].iter().all(|&v| v == 90));
    }

    #[test]
    fn upscale_interpolates() {
        // 2×2 → 4×4: левый столбец 0, правый 255.
        let src = [0, 255, 0, 255, 128, 128];
        let mut buf = vec![0; 4 * 6];
        scale_nv12(&src, 2, 2, &mut Nv12Target { data: &mut buf, pitch: 4, width: 4, height: 4 });
        let row = &buf[0..4];
        assert_eq!(row[0], 0);
        assert_eq!(row[3], 255);
        assert!(row[1] > 0 && row[1] < row[2] && row[2] < 255, "{row:?}");
    }
}
