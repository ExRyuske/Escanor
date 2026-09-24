//! DLL виртуальной камеры «Escanor Camera» для Windows 11.
//!
//! COM-объект (IMFActivate → IMFMediaSourceEx) загружается службой Frame Server,
//! когда какое-то приложение открывает камеру. Кадры берутся из общей памяти,
//! куда их пишет приложение Escanor (см. crate escanor-shm).

pub mod scale;

/// Метка ревизии в самом файле DLL: по ней приложение понимает, изменилась ли камера.
#[used]
static REVISION: &str = concat!("ESCANOR-VCAM-REVISION:", env!("ESCANOR_VCAM_REVISION"), ";");

#[cfg(windows)]
mod activator;
#[cfg(windows)]
mod dll;
#[cfg(windows)]
mod log;
#[cfg(windows)]
mod source;
#[cfg(windows)]
mod stream;
