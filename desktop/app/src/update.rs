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
/// Суффикс для новых файлов, пока они не встали на место старых.
const NEW: &str = "new";
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
/// После успеха нужно запустить новую версию (`start_new_version`) и завершиться.
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
    let mut files = Vec::new();
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        if entry.is_dir() {
            continue;
        }
        let Some(relative) = entry.enclosed_name() else { continue };
        let mut data = Vec::new();
        entry.read_to_end(&mut data)?;
        files.push((dir.join(relative), data));
    }
    replace_all(&files)
}

/// Кладёт новые файлы на место старых целиком или никак: при сбое посередине не остаётся
/// смеси двух версий и нет пропавшего exe.
///
/// Сначала всё записывается рядом (`.new`) — сбой здесь рабочих файлов не касается. Потом
/// подмена: запущенный exe (и adb, если его держит сервер) нельзя перезаписать, но можно
/// переименовать — старый файл уходит в `.old`, новый встаёт на его место. Не вышло — всё
/// уже подменённое возвращается обратно.
fn replace_all(files: &[(PathBuf, Vec<u8>)]) -> Result<()> {
    let written = files.iter().try_for_each(|(target, data)| {
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(side_path(target, NEW), data)
            .with_context(|| format!("не удалось записать {}", target.display()))
    });
    if let Err(e) = written {
        for (target, _) in files {
            let _ = std::fs::remove_file(side_path(target, NEW));
        }
        return Err(e);
    }

    let mut swapped: Vec<(&Path, bool)> = Vec::new();
    for (target, _) in files {
        match swap(target) {
            Ok(had_old) => swapped.push((target, had_old)),
            Err(e) => {
                for &(target, had_old) in swapped.iter().rev() {
                    let _ = std::fs::remove_file(target);
                    if had_old {
                        let _ = std::fs::rename(side_path(target, OLD), target);
                    }
                }
                for (target, _) in files {
                    let _ = std::fs::remove_file(side_path(target, NEW));
                }
                return Err(e).with_context(|| format!("не удалось заменить {}", target.display()));
            }
        }
    }
    Ok(())
}

/// Ставит `.new` на место файла; прежний уходит в `.old`. `true` — прежний был.
fn swap(target: &Path) -> std::io::Result<bool> {
    let (new, old) = (side_path(target, NEW), side_path(target, OLD));
    let had_old = target.exists();
    if had_old {
        let _ = std::fs::remove_file(&old);
        std::fs::rename(target, &old)?;
    }
    if let Err(e) = std::fs::rename(&new, target) {
        if had_old {
            let _ = std::fs::rename(&old, target);
        }
        return Err(e);
    }
    Ok(had_old)
}

/// `escanor.exe` → `escanor.exe.old` / `escanor.exe.new`.
fn side_path(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".");
    name.push(suffix);
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

/// Запускает новую версию; текущей после успеха нужно завершиться. Новая дождётся её выхода.
/// Если запустить не вышло (например, помешал антивирус), текущая должна продолжить работу —
/// иначе не осталось бы ни одной.
pub fn start_new_version() -> Result<()> {
    // Сервер adb запущен из папки программы: пока он работает, старый adb.exe не удалить.
    escanor_core::adb::Adb::locate().kill_server();
    let mut args: Vec<String> =
        std::env::args().skip(1).filter(|a| a != crate::system::TRAY_ARG && a != UPDATED_ARG).collect();
    args.push(UPDATED_ARG.into());
    std::process::Command::new(exe()?).args(args).spawn().context("не удалось запустить новую версию")?;
    Ok(())
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

/// `true` — ни одного `.old` (и недописанного `.new`) не осталось.
fn remove_old_files(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else { return true };
    let mut clean = true;
    for entry in entries.flatten() {
        if entry.path().extension().is_some_and(|e| e == OLD || e == NEW) && std::fs::remove_file(entry.path()).is_err()
        {
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
    fn replaces_all_files_or_none() {
        let dir = std::env::temp_dir().join("escanor-replace-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("escanor.exe"), b"old exe").unwrap();
        std::fs::write(dir.join("adb.exe"), b"old adb").unwrap();
        let read = |name: &str| std::fs::read(dir.join(name)).unwrap();

        replace_all(&[(dir.join("escanor.exe"), b"new exe".to_vec()), (dir.join("app.apk"), b"apk".to_vec())]).unwrap();
        assert_eq!(read("escanor.exe"), b"new exe");
        assert_eq!(read("escanor.exe.old"), b"old exe", "старый exe ждёт удаления после перезапуска");
        assert_eq!(read("app.apk"), b"apk");

        // Второй файл не встаёт на место: старую копию некуда убрать (на месте `.old` — непустая
        // папка). Первый уже подменённый файл возвращается как был.
        std::fs::write(dir.join("blocked"), b"old").unwrap();
        std::fs::create_dir_all(dir.join("blocked.old")).unwrap();
        std::fs::write(dir.join("blocked.old").join("x"), b"").unwrap();
        let result = replace_all(&[(dir.join("adb.exe"), b"new adb".to_vec()), (dir.join("blocked"), b"new".to_vec())]);
        assert!(result.is_err());
        assert_eq!(read("adb.exe"), b"old adb", "откат: прежний файл на месте");
        assert_eq!(read("blocked"), b"old");
        assert!(!dir.join("adb.exe.new").exists() && !dir.join("blocked.new").exists(), "недописанное убрано");

        // Запись не удалась ещё до подмены — рабочие файлы не тронуты вовсе.
        std::fs::write(dir.join("file"), b"").unwrap();
        let result = replace_all(&[(dir.join("adb.exe"), b"x".to_vec()), (dir.join("file").join("y"), b"y".to_vec())]);
        assert!(result.is_err());
        assert_eq!(read("adb.exe"), b"old adb");
        assert!(!dir.join("adb.exe.new").exists());
        let _ = std::fs::remove_dir_all(&dir);
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
        assert_eq!(side_path(Path::new("C:/Escanor/escanor.exe"), OLD), PathBuf::from("C:/Escanor/escanor.exe.old"));
    }
}
