//! Связь с системой: автозапуск, единственный экземпляр, открытие папки.

use std::path::Path;

/// Аргумент, с которым Escanor запускается при входе в Windows: сразу в трей.
pub const TRAY_ARG: &str = "--tray";

pub fn open_folder(path: &Path) {
    let program = if cfg!(windows) {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let _ = std::process::Command::new(program).arg(path).spawn();
}

#[cfg(windows)]
pub use win::{autostart_enabled, set_autostart, take_single_instance};

#[cfg(windows)]
mod win {
    use anyhow::Result;
    use std::time::{Duration, Instant};
    use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError};
    use windows::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_SZ, RRF_RT_REG_SZ, RegCloseKey, RegDeleteValueW,
        RegGetValueW, RegOpenKeyExW, RegSetValueExW,
    };
    use windows::Win32::System::Threading::CreateMutexW;
    use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, SW_RESTORE, SW_SHOW, SetForegroundWindow, ShowWindow};
    use windows::core::{PCWSTR, w};

    const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
    const VALUE: PCWSTR = w!("Escanor");

    fn command() -> Result<String> {
        Ok(format!("\"{}\" {}", std::env::current_exe()?.display(), super::TRAY_ARG))
    }

    fn open(access: windows::Win32::System::Registry::REG_SAM_FLAGS) -> Result<HKEY> {
        let mut key = HKEY::default();
        unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, RUN_KEY, None, access, &mut key).ok()? };
        Ok(key)
    }

    /// Включён ли автозапуск именно этой копии программы (папку могли перенести).
    pub fn autostart_enabled() -> bool {
        let mut buf = [0u16; 1024];
        let mut size = (buf.len() * 2) as u32;
        let status = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                RUN_KEY,
                VALUE,
                RRF_RT_REG_SZ,
                None,
                Some(buf.as_mut_ptr() as *mut _),
                Some(&mut size),
            )
        };
        if status.is_err() {
            return false;
        }
        let value = String::from_utf16_lossy(&buf[..(size as usize / 2).saturating_sub(1)]);
        command().is_ok_and(|c| c.eq_ignore_ascii_case(&value))
    }

    pub fn set_autostart(enabled: bool) -> Result<()> {
        let key = open(KEY_READ | KEY_WRITE)?;
        let result = if enabled {
            let wide: Vec<u16> = command()?.encode_utf16().chain([0]).collect();
            let bytes = unsafe { std::slice::from_raw_parts(wide.as_ptr() as *const u8, wide.len() * 2) };
            unsafe { RegSetValueExW(key, VALUE, None, REG_SZ, Some(bytes)).ok() }
        } else {
            unsafe { RegDeleteValueW(key, VALUE).ok() }.or_else(|_| Ok(()))
        };
        unsafe {
            let _ = RegCloseKey(key);
        }
        Ok(result?)
    }

    /// `false`, если Escanor уже запущен: тогда его окно выводится на передний план.
    /// Два экземпляра стали бы отбирать друг у друга телефон и виртуальную камеру.
    /// `wait_for_previous` — после обновления: старая версия вот-вот завершится, ждём её.
    pub fn take_single_instance(wait_for_previous: bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(15);
        unsafe {
            loop {
                // Мьютекс живёт до конца процесса: дескриптор намеренно не закрывается.
                let handle = CreateMutexW(None, false, w!("Local\\EscanorSingleInstance"));
                if GetLastError() != ERROR_ALREADY_EXISTS {
                    return true;
                }
                // Чужой мьютекс не держим: иначе он не исчезнет и после выхода старой версии.
                if let Ok(handle) = handle {
                    let _ = CloseHandle(handle);
                }
                if !wait_for_previous || Instant::now() >= deadline {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            if let Ok(window) = FindWindowW(PCWSTR::null(), w!("Escanor")) {
                let _ = ShowWindow(window, SW_SHOW);
                let _ = ShowWindow(window, SW_RESTORE);
                let _ = SetForegroundWindow(window);
            }
            false
        }
    }
}

#[cfg(not(windows))]
pub fn autostart_enabled() -> bool {
    false
}

#[cfg(not(windows))]
pub fn set_autostart(_: bool) -> anyhow::Result<()> {
    anyhow::bail!("автозапуск настраивается только в Windows")
}

#[cfg(not(windows))]
pub fn take_single_instance(_wait_for_previous: bool) -> bool {
    true
}
