//! Журнал DLL: служба Frame Server работает в фоне, и без журнала её ошибки не увидеть.
//! Файл — %ProgramData%\Escanor\vcam.log; приложение показывает последнюю строку.

use std::fmt::Arguments;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use windows::Win32::System::SystemInformation::GetLocalTime;

const MAX_BYTES: u64 = 512 * 1024;

static LOCK: Mutex<()> = Mutex::new(());

fn path() -> PathBuf {
    let base = std::env::var_os("ProgramData").map_or_else(|| PathBuf::from("C:\\ProgramData"), PathBuf::from);
    base.join("Escanor").join("vcam.log")
}

pub fn write(args: Arguments) {
    let _guard = LOCK.lock();
    let path = path();
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > MAX_BYTES) {
        let _ = std::fs::rename(&path, path.with_extension("old.log"));
    }
    let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(&path) else { return };
    let t = unsafe { GetLocalTime() };
    let _ = writeln!(
        file,
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03} [{}] {args}",
        t.wYear,
        t.wMonth,
        t.wDay,
        t.wHour,
        t.wMinute,
        t.wSecond,
        t.wMilliseconds,
        std::process::id()
    );
}

macro_rules! vlog {
    ($($arg:tt)*) => { $crate::log::write(format_args!($($arg)*)) };
}
pub(crate) use vlog;

/// Счётчик живых COM-объектов: DLL можно выгрузить (и обновить), только когда он равен нулю.
pub static LIVE_OBJECTS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

pub struct Live;

impl Live {
    pub fn new() -> Self {
        LIVE_OBJECTS.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        Live
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        LIVE_OBJECTS.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
    }
}
