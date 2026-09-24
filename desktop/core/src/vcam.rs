//! Виртуальная камера Windows 11: регистрация через `MFCreateVirtualCamera`
//! и передача кадров в DLL-источник через общую память.
//! На других ОС — заглушки, чтобы остальной код собирался и работал (превью, звук).

use crate::decoder::Nv12Frame;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VcamStatus {
    /// Не Windows: виртуальной камеры нет.
    Unsupported,
    /// DLL не зарегистрирована — нужна однократная установка с правами администратора.
    NotInstalled,
    /// Зарегистрирована старая версия или DLL лежит не в Program Files.
    Outdated,
    /// Камера видна в системе, но её никто не открыл.
    Ready,
    /// Видео выключено в Escanor — камера убрана из системы, приложения её не держат.
    Off,
    /// Какое-то приложение получает видео в этом разрешении.
    InUse {
        width: u32,
        height: u32,
    },
    Failed(String),
}

/// Приёмник декодированных кадров для DLL камеры. Пишет только когда камеру кто-то открыл.
#[derive(Default)]
pub struct FrameOutput {
    #[cfg(windows)]
    inner: std::sync::Mutex<win::Output>,
}

impl FrameOutput {
    pub fn write(&self, _planes: &Nv12Frame) {
        #[cfg(windows)]
        self.inner.lock().unwrap().write(_planes);
    }

    /// Разрешение, в котором камеру сейчас читает какое-то приложение.
    pub fn consumer(&self) -> Option<(u32, u32)> {
        #[cfg(windows)]
        return self.inner.lock().unwrap().consumer();
        #[cfg(not(windows))]
        None
    }
}

#[cfg(not(windows))]
pub struct VirtualCamera;

#[cfg(not(windows))]
impl VirtualCamera {
    pub fn start() -> anyhow::Result<Self> {
        anyhow::bail!("виртуальная камера пока есть только для Windows")
    }

    pub fn set_enabled(&self, _: bool) -> anyhow::Result<()> {
        Ok(())
    }
}

#[cfg(not(windows))]
pub fn install() -> anyhow::Result<()> {
    anyhow::bail!("виртуальная камера пока есть только для Windows")
}

#[cfg(not(windows))]
pub fn run_install() -> anyhow::Result<()> {
    anyhow::bail!("виртуальная камера пока есть только для Windows")
}

#[cfg(not(windows))]
pub fn installation() -> Installation {
    Installation::Missing
}

#[cfg(not(windows))]
pub fn last_log_line() -> Option<String> {
    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Installation {
    Missing,
    Outdated,
    Current,
}

pub fn initial_status() -> VcamStatus {
    if !cfg!(windows) {
        return VcamStatus::Unsupported;
    }
    match installation() {
        Installation::Missing => VcamStatus::NotInstalled,
        Installation::Outdated => VcamStatus::Outdated,
        Installation::Current => VcamStatus::Ready,
    }
}

#[cfg(windows)]
pub use win::{VirtualCamera, install, installation, last_log_line, run_install};

#[cfg(windows)]
mod win {
    use super::Installation;
    use crate::decoder::Nv12Frame;
    use anyhow::{Context, Result, bail};
    use escanor_shm::windows::Mapping;
    use escanor_shm::{MAX_HEIGHT, MAX_WIDTH, VCAM_CLSID_STR, VCAM_FRIENDLY_NAME};
    use std::path::PathBuf;
    use std::process::Command;
    use std::time::{Duration, Instant};
    use windows::Win32::Foundation::WIN32_ERROR;
    use windows::Win32::Foundation::{CloseHandle, ERROR_SUCCESS};
    use windows::Win32::Media::MediaFoundation::{
        IMFVirtualCamera, MF_VERSION, MFCreateVirtualCamera, MFSTARTUP_FULL, MFStartup,
        MFVirtualCameraAccess_CurrentUser, MFVirtualCameraLifetime_Session, MFVirtualCameraType_SoftwareCameraSource,
    };
    use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
    use windows::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW};
    use windows::Win32::System::Threading::{GetExitCodeProcess, INFINITE, WaitForSingleObject};
    use windows::Win32::UI::Shell::{SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW};
    use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;
    use windows::core::{HSTRING, PCWSTR, w};

    /// Как часто пытаться открыть общую память, пока DLL её не создала.
    const RETRY_OPEN: Duration = Duration::from_millis(500);

    #[derive(Default)]
    pub(super) struct Output {
        mapping: Option<Mapping>,
        last_attempt: Option<Instant>,
    }

    impl Output {
        fn ensure_mapping(&mut self) -> Option<&Mapping> {
            if self.mapping.is_none() && self.last_attempt.is_none_or(|t| t.elapsed() >= RETRY_OPEN) {
                self.last_attempt = Some(Instant::now());
                self.mapping = Mapping::open().ok().filter(|m| m.frames().is_compatible());
            }
            self.mapping.as_ref()
        }

        pub(super) fn write(&mut self, planes: &Nv12Frame) {
            let (w, h) = (planes.width as u32, planes.height as u32);
            if w > MAX_WIDTH || h > MAX_HEIGHT {
                return;
            }
            let Some(mapping) = self.ensure_mapping() else { return };
            let frames = mapping.frames();
            if frames.consumer().is_none() {
                return;
            }
            frames.write(w, h, |buf| planes.write_nv12(buf));
        }

        pub(super) fn consumer(&mut self) -> Option<(u32, u32)> {
            self.ensure_mapping()?.frames().consumer()
        }
    }

    /// Зарегистрированная на время сессии камера «Escanor Camera».
    /// Пропадает из системы, когда объект уничтожен или процесс завершён.
    pub struct VirtualCamera {
        camera: IMFVirtualCamera,
    }

    impl VirtualCamera {
        /// Вызывать на потоке, который живёт столько же, сколько камера (поток движка).
        pub fn start() -> Result<Self> {
            unsafe {
                // S_FALSE/RPC_E_CHANGED_MODE — COM уже инициализирован, это нормально.
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
                MFStartup(MF_VERSION, MFSTARTUP_FULL).context("MFStartup")?;
                let camera = MFCreateVirtualCamera(
                    MFVirtualCameraType_SoftwareCameraSource,
                    MFVirtualCameraLifetime_Session,
                    MFVirtualCameraAccess_CurrentUser,
                    &HSTRING::from(VCAM_FRIENDLY_NAME),
                    &HSTRING::from(VCAM_CLSID_STR),
                    None,
                )
                .context("MFCreateVirtualCamera")?;
                camera.Start(None).context("IMFVirtualCamera::Start")?;
                Ok(Self { camera })
            }
        }

        /// Выключенная камера пропадает из системы: приложения закрывают поток, индикатор
        /// «камера используется» гаснет, никто больше не рисует кадры-заглушки.
        pub fn set_enabled(&self, enabled: bool) -> Result<()> {
            unsafe {
                if enabled {
                    self.camera.Start(None).context("IMFVirtualCamera::Start")
                } else {
                    self.camera.Stop().context("IMFVirtualCamera::Stop")
                }
            }
        }
    }

    impl Drop for VirtualCamera {
        fn drop(&mut self) {
            unsafe {
                let _ = self.camera.Shutdown();
            }
        }
    }

    const DLL_NAME: &str = "escanor_vcam.dll";
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    /// Служба Frame Server работает под LOCAL SERVICE и не может читать папки пользователя
    /// (Загрузки, Рабочий стол), поэтому DLL устанавливается в Program Files.
    fn install_dir() -> PathBuf {
        let base = std::env::var_os("ProgramFiles").map_or_else(|| PathBuf::from("C:\\Program Files"), PathBuf::from);
        base.join("Escanor")
    }

    fn data_dir() -> PathBuf {
        let base = std::env::var_os("ProgramData").map_or_else(|| PathBuf::from("C:\\ProgramData"), PathBuf::from);
        base.join("Escanor")
    }

    fn installed_dll() -> PathBuf {
        install_dir().join(DLL_NAME)
    }

    /// DLL, которая лежит рядом с программой (из дистрибутива).
    fn bundled_dll() -> Option<PathBuf> {
        let path = std::env::current_exe().ok()?.with_file_name(DLL_NAME);
        path.is_file().then_some(path)
    }

    fn registered_path() -> Option<String> {
        let key = HSTRING::from(format!("SOFTWARE\\Classes\\CLSID\\{VCAM_CLSID_STR}\\InprocServer32"));
        let mut buf = [0u16; 1024];
        let mut size = (buf.len() * 2) as u32;
        let status: WIN32_ERROR = unsafe {
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                &key,
                PCWSTR::null(),
                RRF_RT_REG_SZ,
                None,
                Some(buf.as_mut_ptr() as *mut _),
                Some(&mut size),
            )
        };
        if status != ERROR_SUCCESS {
            return None;
        }
        let len = (size as usize / 2).saturating_sub(1);
        Some(String::from_utf16_lossy(&buf[..len]))
    }

    pub fn installation() -> Installation {
        let Some(registered) = registered_path() else { return Installation::Missing };
        let target = installed_dll();
        if !registered.eq_ignore_ascii_case(&target.to_string_lossy()) || !target.is_file() {
            return Installation::Outdated;
        }
        if let Some(bundled) = bundled_dll()
            && !same_camera(&bundled, &target)
        {
            return Installation::Outdated;
        }
        Installation::Current
    }

    /// Одна ли это камера. Файлы DLL отличаются при каждой сборке, поэтому сравнивается ревизия —
    /// хеш исходников камеры; у DLL без неё (старых версий) — файлы целиком.
    fn same_camera(bundled: &std::path::Path, installed: &std::path::Path) -> bool {
        let (Ok(a), Ok(b)) = (std::fs::read(bundled), std::fs::read(installed)) else { return false };
        match (escanor_shm::vcam_revision(&a), escanor_shm::vcam_revision(&b)) {
            (Some(a), Some(b)) => a == b,
            _ => a == b,
        }
    }

    /// Последняя строка журнала DLL (%ProgramData%\\Escanor\\vcam.log).
    pub fn last_log_line() -> Option<String> {
        let text = std::fs::read_to_string(data_dir().join("vcam.log")).ok()?;
        text.lines().rev().find(|l| !l.trim().is_empty()).map(str::to_string)
    }

    /// Перезапускает программу с правами администратора (UAC) в режиме установки камеры.
    pub fn install() -> Result<()> {
        bundled_dll().with_context(|| format!("рядом с программой нет {DLL_NAME}"))?;
        let exe = HSTRING::from(std::env::current_exe()?.as_os_str());
        let mut info = SHELLEXECUTEINFOW {
            cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOCLOSEPROCESS,
            lpVerb: w!("runas"),
            lpFile: PCWSTR(exe.as_ptr()),
            lpParameters: w!("--install-vcam"),
            nShow: SW_HIDE.0,
            ..Default::default()
        };
        unsafe {
            ShellExecuteExW(&mut info).context("установка отклонена")?;
            WaitForSingleObject(info.hProcess, INFINITE);
            let mut code = 0u32;
            let _ = GetExitCodeProcess(info.hProcess, &mut code);
            let _ = CloseHandle(info.hProcess);
            if code != 0 {
                let details = std::fs::read_to_string(data_dir().join("install-error.txt")).unwrap_or_default();
                bail!("установка не удалась (код {code}): {}", details.trim());
            }
        }
        Ok(())
    }

    /// Выполняется в процессе с правами администратора (`escanor.exe --install-vcam`).
    /// Текст ошибки сохраняется в файл: у этого процесса нет окна, а родитель видит только код выхода.
    pub fn run_install() -> Result<()> {
        let error_file = data_dir().join("install-error.txt");
        let _ = std::fs::remove_file(&error_file);
        let result = install_files();
        if let Err(e) = &result {
            let _ = std::fs::create_dir_all(data_dir());
            let _ = std::fs::write(&error_file, format!("{e:#}"));
        }
        result
    }

    fn install_files() -> Result<()> {
        let source = bundled_dll().with_context(|| format!("рядом с программой нет {DLL_NAME}"))?;
        let dir = install_dir();
        std::fs::create_dir_all(&dir).with_context(|| format!("не создать {}", dir.display()))?;

        // Старую DLL может держать служба камеры. Загруженный файл можно переименовать,
        // но не перезаписать — переименовываем и кладём новый рядом.
        let target = installed_dll();
        if target.exists() {
            let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
            let _ = std::fs::rename(&target, dir.join(format!("escanor_vcam.{stamp}.old")));
        }
        std::fs::copy(&source, &target).with_context(|| format!("не скопировать в {}", target.display()))?;
        for entry in std::fs::read_dir(&dir)?.flatten() {
            if entry.path().extension().is_some_and(|e| e == "old") {
                // Занятые файлы удалятся при следующей установке.
                let _ = std::fs::remove_file(entry.path());
            }
        }

        // Журнал DLL: писать в него должна служба (LOCAL SERVICE), читать — приложение.
        let data = data_dir();
        std::fs::create_dir_all(&data)?;
        run(Command::new("icacls").arg(&data).args([
            "/grant",
            "*S-1-5-19:(OI)(CI)M",
            "*S-1-5-32-545:(OI)(CI)M",
            "/Q",
        ]))?;

        run(Command::new("regsvr32").arg("/s").arg(&target)).context("regsvr32")?;
        Ok(())
    }

    fn run(command: &mut Command) -> Result<()> {
        use std::os::windows::process::CommandExt;
        let status = command.creation_flags(CREATE_NO_WINDOW).status()?;
        if !status.success() {
            bail!("{command:?}: код {}", status.code().unwrap_or(-1));
        }
        Ok(())
    }
}
