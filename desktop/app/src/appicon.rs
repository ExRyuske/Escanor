//! Иконка программы для кнопки макропада: та же, что показывает Проводник.

use std::path::Path;

/// Иконка exe, ярлыка или любого файла в наибольшем размере, который знает Windows (до 256 px).
#[cfg(windows)]
pub fn extract(path: &Path) -> Option<image::RgbaImage> {
    use windows::Win32::Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES;
    use windows::Win32::UI::Controls::{IImageList, ILD_TRANSPARENT};
    use windows::Win32::UI::Shell::{SHFILEINFOW, SHGFI_SYSICONINDEX, SHGetFileInfoW, SHGetImageList, SHIL_JUMBO};
    use windows::Win32::UI::WindowsAndMessaging::DestroyIcon;
    use windows::core::HSTRING;

    unsafe {
        let mut info = SHFILEINFOW::default();
        let file = HSTRING::from(path.as_os_str());
        let found = SHGetFileInfoW(
            &file,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            Some(&mut info),
            size_of::<SHFILEINFOW>() as u32,
            SHGFI_SYSICONINDEX,
        );
        if found == 0 {
            return None;
        }
        let list: IImageList = SHGetImageList(SHIL_JUMBO as i32).ok()?;
        let icon = list.GetIcon(info.iIcon, ILD_TRANSPARENT.0).ok()?;
        let image = icon_to_rgba(icon);
        let _ = DestroyIcon(icon);
        image.map(trim)
    }
}

#[cfg(not(windows))]
pub fn extract(_: &Path) -> Option<image::RgbaImage> {
    None
}

#[cfg(windows)]
unsafe fn icon_to_rgba(icon: windows::Win32::UI::WindowsAndMessaging::HICON) -> Option<image::RgbaImage> {
    use windows::Win32::Graphics::Gdi::{
        BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, DeleteObject, GetDC, GetDIBits, GetObjectW,
        ReleaseDC,
    };
    use windows::Win32::UI::WindowsAndMessaging::{GetIconInfo, ICONINFO};

    unsafe {
        let mut info = ICONINFO::default();
        GetIconInfo(icon, &mut info).ok()?;
        let mut bitmap = BITMAP::default();
        GetObjectW(info.hbmColor.into(), size_of::<BITMAP>() as i32, Some(&mut bitmap as *mut BITMAP as *mut _));
        let (width, height) = (bitmap.bmWidth, bitmap.bmHeight);
        let mut pixels = vec![0u8; (width.max(0) * height.max(0) * 4) as usize];
        let mut header = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                // Отрицательная высота — строки сверху вниз.
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let dc = GetDC(None);
        let lines = GetDIBits(
            dc,
            info.hbmColor,
            0,
            height as u32,
            Some(pixels.as_mut_ptr() as *mut _),
            &mut header,
            DIB_RGB_COLORS,
        );
        ReleaseDC(None, dc);
        let _ = DeleteObject(info.hbmColor.into());
        let _ = DeleteObject(info.hbmMask.into());
        if lines == 0 || width <= 0 || height <= 0 {
            return None;
        }
        // BGRA → RGBA. У старых иконок без альфа-канала он весь нулевой — тогда они непрозрачны.
        let opaque = pixels.as_chunks::<4>().0.iter().all(|p| p[3] == 0);
        for p in pixels.as_chunks_mut::<4>().0 {
            p.swap(0, 2);
            if opaque {
                p[3] = 255;
            }
        }
        image::RgbaImage::from_raw(width as u32, height as u32, pixels)
    }
}

/// Обрезает прозрачные поля (у старых программ маленькая иконка лежит в углу большого холста)
/// и выравнивает по центру квадрата.
#[cfg_attr(not(windows), allow(dead_code))]
fn trim(image: image::RgbaImage) -> image::RgbaImage {
    let (w, h) = image.dimensions();
    let visible = |x: u32, y: u32| image.get_pixel(x, y)[3] > 8;
    let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0, 0);
    for y in 0..h {
        for x in 0..w {
            if visible(x, y) {
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
            }
        }
    }
    if x0 > x1 || y0 > y1 {
        return image;
    }
    let (cw, ch) = (x1 - x0 + 1, y1 - y0 + 1);
    let side = cw.max(ch);
    let mut out = image::RgbaImage::new(side, side);
    image::imageops::overlay(
        &mut out,
        &image::imageops::crop_imm(&image, x0, y0, cw, ch).to_image(),
        ((side - cw) / 2) as i64,
        ((side - ch) / 2) as i64,
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_empty_border() {
        let mut image = image::RgbaImage::new(256, 256);
        for y in 0..32 {
            for x in 0..16 {
                image.put_pixel(x, y, image::Rgba([255, 0, 0, 255]));
            }
        }
        let out = trim(image);
        assert_eq!(out.dimensions(), (32, 32), "обрезано до содержимого и выровнено в квадрат");
        assert_eq!(out.get_pixel(16, 16)[3], 255);
        assert_eq!(out.get_pixel(0, 0)[3], 0, "по бокам прозрачные поля");
    }
}
