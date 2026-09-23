//! Иконка внутри escanor.exe: Проводник и панель задач берут её из ресурса файла,
//! а не из работающей программы. Рисуется тем же кодом, что трей и окно.

#[path = "src/icon.rs"]
mod icon;

fn main() {
    println!("cargo:rerun-if-changed=src/icon.rs");
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").expect("нет OUT_DIR"));
    let ico = out.join("escanor.ico");
    if let Err(e) = std::fs::write(&ico, build_ico(&[16, 24, 32, 48, 64, 256])) {
        println!("cargo:warning=иконка не записана: {e}");
        return;
    }
    let rc = out.join("escanor.rc");
    let contents = format!("1 ICON \"{}\"\n", ico.display().to_string().replace('\\', "\\\\"));
    if let Err(e) = std::fs::write(&rc, contents) {
        println!("cargo:warning=ресурс не записан: {e}");
        return;
    }
    // Компилятора ресурсов может не быть (например, при кросс-сборке) — это не повод ронять сборку.
    embed_resource::compile(&rc, embed_resource::NONE)
        .manifest_optional()
        .unwrap_or_else(|e| println!("cargo:warning=иконка в exe не встроена: {e}"));
}

/// Многоразмерный .ico: оглавление и DIB с альфа-каналом для каждого размера.
fn build_ico(sizes: &[u32]) -> Vec<u8> {
    let images: Vec<Vec<u8>> = sizes.iter().map(|&s| build_dib(s)).collect();
    let mut out = Vec::new();
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&(sizes.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * sizes.len() as u32;
    for (&size, image) in sizes.iter().zip(&images) {
        // 256 записывается нулём: под размер в формате один байт.
        let byte = if size >= 256 { 0 } else { size as u8 };
        out.extend_from_slice(&[byte, byte, 0, 0]);
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&32u16.to_le_bytes());
        out.extend_from_slice(&(image.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += image.len() as u32;
    }
    for image in images {
        out.extend_from_slice(&image);
    }
    out
}

fn build_dib(size: u32) -> Vec<u8> {
    let rgba = icon::render(size);
    let mut out = Vec::new();
    // BITMAPINFOHEADER; высота удвоена — формат ждёт изображение и маску.
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&size.to_le_bytes());
    out.extend_from_slice(&(size * 2).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&[0u8; 24]);
    // Пиксели снизу вверх, BGRA.
    for y in (0..size).rev() {
        for x in 0..size {
            let i = ((y * size + x) * 4) as usize;
            out.extend_from_slice(&[rgba[i + 2], rgba[i + 1], rgba[i], rgba[i + 3]]);
        }
    }
    // Пустая маска прозрачности, строки выровнены по 4 байта.
    let mask_row = size.div_ceil(8).div_ceil(4) * 4;
    out.extend(std::iter::repeat_n(0u8, (mask_row * size) as usize));
    out
}
