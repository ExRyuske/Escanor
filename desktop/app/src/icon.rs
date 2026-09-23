//! Иконка приложения: фиолетовый объектив на тёмной плашке, в цветах YeruVerse.
//!
//! Рисуется кодом, а не хранится файлом: так трей, окно и ресурс внутри exe
//! (см. build.rs) получают одну и ту же картинку любого размера. Модуль зависит
//! только от стандартной библиотеки, чтобы его можно было подключить в build.rs.

/// RGBA, `size`×`size`.
pub fn render(size: u32) -> Vec<u8> {
    let s = size as f32;
    let mut out = vec![0u8; (size * size * 4) as usize];
    for y in 0..size {
        for x in 0..size {
            // Центр пикселя в долях от стороны, начало координат — в центре.
            let px = (x as f32 + 0.5) / s - 0.5;
            let py = (y as f32 + 0.5) / s - 0.5;
            let aa = 1.0 / s;

            // Скруглённая плашка.
            let half = 0.5 - aa;
            let radius = 0.22;
            let qx = (px.abs() - (half - radius)).max(0.0);
            let qy = (py.abs() - (half - radius)).max(0.0);
            let plate = coverage((qx * qx + qy * qy).sqrt() - radius, aa);

            let d = (px * px + py * py).sqrt();
            let ring = coverage(d - 0.33, aa); // объектив
            let lens = coverage(d - 0.19, aa); // тёмное стекло
            let hx = px + 0.07;
            let hy = py + 0.07;
            let glint = coverage((hx * hx + hy * hy).sqrt() - 0.055, aa); // блик

            let mut color = [0x17 as f32, 0x1A as f32, 0x21 as f32];
            color = mix(color, [0x8B as f32, 0x5C as f32, 0xF6 as f32], ring);
            color = mix(color, [0x0F as f32, 0x11 as f32, 0x15 as f32], lens);
            color = mix(color, [0xA7 as f32, 0x8B as f32, 0xFA as f32], glint);

            let i = ((y * size + x) * 4) as usize;
            out[i] = color[0].round() as u8;
            out[i + 1] = color[1].round() as u8;
            out[i + 2] = color[2].round() as u8;
            out[i + 3] = (plate * 255.0).round() as u8;
        }
    }
    out
}

/// Доля пикселя внутри фигуры по расстоянию до её края (отрицательное — внутри).
fn coverage(distance: f32, aa: f32) -> f32 {
    (0.5 - distance / aa).clamp(0.0, 1.0)
}

fn mix(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
}
