//! Запуск программы, файла или ссылки по кнопке макропада — так же, как двойной щелчок
//! в Проводнике: exe, ярлык .lnk, документ или адрес сайта.

use anyhow::Result;

#[cfg(windows)]
pub fn launch(target: &str) -> Result<()> {
    use std::path::Path;
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    use windows::core::{HSTRING, PCWSTR, w};

    // Рабочая папка — папка программы: многие программы ищут свои файлы рядом с собой.
    let directory = Path::new(target).parent().filter(|p| p.is_dir()).map(|p| HSTRING::from(p.as_os_str()));
    let file = HSTRING::from(target);
    let result = unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            &file,
            PCWSTR::null(),
            directory.as_ref().map_or(PCWSTR::null(), |d| PCWSTR(d.as_ptr())),
            SW_SHOWNORMAL,
        )
    };
    // По договорённости ShellExecute значения до 32 — коды ошибок.
    if result.0 as usize <= 32 {
        anyhow::bail!("не удалось открыть {target} (код {})", result.0 as usize);
    }
    Ok(())
}

#[cfg(target_os = "macos")]
pub fn launch(target: &str) -> Result<()> {
    std::process::Command::new("open").arg(target).spawn()?;
    Ok(())
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn launch(target: &str) -> Result<()> {
    std::process::Command::new("xdg-open").arg(target).spawn()?;
    Ok(())
}
