//! Ревизия камеры — хеш исходников DLL и общей памяти. Приложение сравнивает её у установленной
//! и у новой DLL: сами файлы отличаются при каждой сборке (время сборки, номер версии), и без
//! ревизии камера считалась бы устаревшей после любого обновления программы.

use std::path::{Path, PathBuf};

fn main() {
    let roots = ["src", "Cargo.toml", "../shm/src", "../shm/Cargo.toml"];
    let mut files = Vec::new();
    for root in roots {
        println!("cargo:rerun-if-changed={root}");
        collect(Path::new(root), &mut files);
    }
    files.sort();
    // FNV-1a: хватает, чтобы заметить любую правку, и не нужны зависимости.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for file in &files {
        let name = file.to_string_lossy().replace('\\', "/");
        let text = std::fs::read(file).expect("исходник камеры");
        for byte in name.bytes().chain([0]).chain(text).chain([0]) {
            hash = (hash ^ byte as u64).wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    println!("cargo:rustc-env=ESCANOR_VCAM_REVISION={hash:016x}");
}

fn collect(path: &Path, files: &mut Vec<PathBuf>) {
    if path.is_dir() {
        for entry in std::fs::read_dir(path).expect("папка исходников").flatten() {
            collect(&entry.path(), files);
        }
    } else if path.is_file() {
        files.push(path.to_path_buf());
    }
}
