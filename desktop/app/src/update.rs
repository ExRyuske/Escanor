//! Обновление из релизов GitHub, как в YeruVerse: проверить последнюю версию,
//! скачать пакет, сверить контрольную сумму, заменить файлы и перезапуститься.
//!
//! Пакет — zip со всем содержимым папки программы (exe, DLL веб-камеры, APK, adb).
//! Настройки и картинки макропада в пакет не входят и остаются на месте.

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

pub const REPO: &str = "ExRyuske/Escanor";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// С этим аргументом запускается новая версия сразу после обновления: она дожидается
/// завершения старой и удаляет оставшиеся от неё файлы.
pub const UPDATED_ARG: &str = "--updated";

/// Суффикс для файлов, которые заменены обновлением, но ещё заняты работающей программой.
const OLD: &str = "old";
/// Пакет больше не бывает: защита от бесконечной загрузки.
const MAX_PACKAGE: u64 = 300 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub struct Release {
    pub version: String,
    pub page: String,
    package: String,
    checksum: Option<String>,
}

#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
    html_url: String,
    assets: Vec<GithubAsset>,
}

#[derive(Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
}

fn get(url: &str) -> Result<ureq::Body> {
    let response = ureq::get(url)
        .header("User-Agent", concat!("Escanor/", env!("CARGO_PKG_VERSION")))
        .header("Accept", "application/vnd.github+json")
        .call()
        .with_context(|| format!("запрос {url}"))?;
    Ok(response.into_body())
}

/// Версия новее установленной или `None`.
pub fn check() -> Result<Option<Release>> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let response = ureq::get(&url).header("User-Agent", concat!("Escanor/", env!("CARGO_PKG_VERSION"))).call();
    let release: GithubRelease = match response {
        Ok(response) => response.into_body().read_json().context("непонятный ответ GitHub")?,
        // Релизов ещё нет — значит, и обновляться не на что.
        Err(ureq::Error::StatusCode(404)) => return Ok(None),
        Err(ureq::Error::StatusCode(403 | 429)) => bail!("GitHub временно ограничил запросы — попробуйте позже"),
        Err(ureq::Error::StatusCode(code)) => bail!("GitHub ответил ошибкой {code}"),
        Err(_) => bail!("нет связи с GitHub"),
    };
    let version = release.tag_name.trim_start_matches('v').to_string();
    if !is_newer(&version, VERSION) {
        return Ok(None);
    }
    let name = package_name(&version);
    let find = |name: &str| release.assets.iter().find(|a| a.name == name).map(|a| a.browser_download_url.clone());
    let package = find(&name).with_context(|| format!("в релизе {version} нет {name}"))?;
    Ok(Some(Release { checksum: find(&format!("{name}.sha256")), version, page: release.html_url, package }))
}

pub fn package_name(version: &str) -> String {
    format!("escanor-{version}-windows-x86_64.zip")
}

fn parse_version(value: &str) -> Option<(u64, u64, u64)> {
    let mut parts = value.split('.').map(|p| p.parse::<u64>().ok());
    Some((parts.next()??, parts.next()??, parts.next()??))
}

fn is_newer(candidate: &str, current: &str) -> bool {
    matches!((parse_version(candidate), parse_version(current)), (Some(a), Some(b)) if a > b)
}

/// Скачивает пакет, проверяет его и раскладывает по папке программы.
/// После успеха нужно перезапуститься (`restart`).
pub fn install(release: &Release) -> Result<()> {
    if !cfg!(windows) {
        bail!("пакет обновления собирается только для Windows");
    }
    let dir = program_dir()?;
    let mut package = Vec::new();
    get(&release.package)?.with_config().limit(MAX_PACKAGE).reader().read_to_end(&mut package)?;

    if let Some(url) = &release.checksum {
        let expected = get(url)?.read_to_string()?;
        let expected = expected.split_whitespace().next().unwrap_or_default().to_lowercase();
        let actual: String = Sha256::digest(&package).iter().map(|b| format!("{b:02x}")).collect();
        if expected != actual {
            bail!("контрольная сумма пакета не совпала — файл повреждён или подменён");
        }
    } else {
        bail!("в релизе нет контрольной суммы пакета — устанавливать без проверки небезопасно");
    }

    let mut archive = zip::ZipArchive::new(Cursor::new(package)).context("пакет не zip")?;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        if entry.is_dir() {
            continue;
        }
        let Some(relative) = entry.enclosed_name() else { continue };
        let target = dir.join(relative);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut data = Vec::new();
        entry.read_to_end(&mut data)?;
        replace(&target, &data).with_context(|| format!("не удалось заменить {}", target.display()))?;
    }
    Ok(())
}

/// Запущенный exe (и adb, если его держит сервер) нельзя перезаписать, но можно
/// переименовать: старый файл уходит в `.old`, новый ложится на его место.
fn replace(target: &Path, data: &[u8]) -> std::io::Result<()> {
    if target.exists() {
        let old = old_path(target);
        let _ = std::fs::remove_file(&old);
        std::fs::rename(target, &old)?;
    }
    std::fs::write(target, data)
}

fn old_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".");
    name.push(OLD);
    path.with_file_name(name)
}

/// Путь к exe, запомненный при запуске (первый вызов — из `cleanup` в `main`). После обновления
/// работающий файл переименован в `.old`, а запускать нужно новый — по прежнему пути.
fn exe() -> Result<PathBuf> {
    static EXE: OnceLock<Option<PathBuf>> = OnceLock::new();
    EXE.get_or_init(|| std::env::current_exe().ok()).clone().context("не найден exe программы")
}

fn program_dir() -> Result<PathBuf> {
    Ok(exe()?.parent().context("нет папки программы")?.to_path_buf())
}

/// Запускает новую версию и завершает текущую.
pub fn restart() -> ! {
    // Сервер adb запущен из папки программы: пока он работает, старый adb.exe не удалить.
    escanor_core::adb::Adb::locate().kill_server();
    if let Ok(exe) = exe() {
        let mut args: Vec<String> =
            std::env::args().skip(1).filter(|a| a != crate::system::TRAY_ARG && a != UPDATED_ARG).collect();
        args.push(UPDATED_ARG.into());
        let _ = std::process::Command::new(exe).args(args).spawn();
    }
    std::process::exit(0)
}

/// Удаляет файлы, оставшиеся от прошлого обновления. Сразу после обновления старая версия ещё
/// может завершаться и держать свои файлы — тогда пробуем ещё несколько секунд в фоне.
pub fn cleanup(after_update: bool) {
    let Ok(dir) = program_dir() else { return };
    if remove_old_files(&dir) || !after_update {
        return;
    }
    std::thread::spawn(move || {
        for _ in 0..40 {
            std::thread::sleep(Duration::from_millis(250));
            if remove_old_files(&dir) {
                return;
            }
        }
    });
}

/// `true` — ни одного `.old` не осталось.
fn remove_old_files(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else { return true };
    let mut clean = true;
    for entry in entries.flatten() {
        if entry.path().extension().is_some_and(|e| e == OLD) && std::fs::remove_file(entry.path()).is_err() {
            clean = false;
        }
    }
    clean
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions() {
        assert!(is_newer("0.2.0", "0.1.9"));
        assert!(is_newer("1.0.0", "0.9.9"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("0.1.0", "0.2.0"));
        assert!(!is_newer("abc", "0.1.0"));
    }

    #[test]
    fn removes_only_old_files() {
        let dir = std::env::temp_dir().join("escanor-update-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["escanor.exe", "escanor.exe.old", "adb.exe.old"] {
            std::fs::write(dir.join(name), b"").unwrap();
        }
        assert!(remove_old_files(&dir));
        let left: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(left, ["escanor.exe"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn old_file_names() {
        assert_eq!(old_path(Path::new("C:/Escanor/escanor.exe")), PathBuf::from("C:/Escanor/escanor.exe.old"));
    }
}
