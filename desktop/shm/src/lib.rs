//! Общая память между приложением Escanor и DLL виртуальной камеры.
//!
//! Приложение пишет декодированные кадры NV12, DLL (внутри службы Windows Frame Server)
//! забирает самый свежий. Три слота + seqlock на каждом: писатель никогда не ждёт читателя,
//! а читатель отбрасывает кадр, если его перезаписали во время копирования.
//!
//! Сама логика не зависит от ОС и тестируется на обычной памяти; создание именованного
//! отображения — только для Windows (модуль `windows`).

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering, fence};
use std::time::{SystemTime, UNIX_EPOCH};

/// COM-класс источника виртуальной камеры (DLL `escanor_vcam.dll`).
pub const VCAM_CLSID: u128 = 0x411C5F8F_DE2E_4C57_817F_FB1C321CA459;
pub const VCAM_CLSID_STR: &str = "{411C5F8F-DE2E-4C57-817F-FB1C321CA459}";
pub const VCAM_FRIENDLY_NAME: &str = "Escanor Camera";

/// Ревизия камеры, записанная в файл DLL (`ESCANOR-VCAM-REVISION:<хеш>;`); `None` — метки нет
/// (DLL старше этой метки).
pub fn vcam_revision(dll: &[u8]) -> Option<&[u8]> {
    const MARKER: &[u8] = b"ESCANOR-VCAM-REVISION:";
    let start = dll.windows(MARKER.len()).position(|w| w == MARKER)? + MARKER.len();
    let length = dll[start..].iter().take(32).position(|&b| b == b';')?;
    Some(&dll[start..start + length])
}

pub const MAGIC: u32 = u32::from_le_bytes(*b"ESCV");
pub const VERSION: u32 = 1;
pub const MAX_WIDTH: u32 = 3840;
pub const MAX_HEIGHT: u32 = 2160;
pub const SLOT_COUNT: usize = 3;
pub const MAX_FRAME_BYTES: usize = nv12_size(MAX_WIDTH, MAX_HEIGHT);
pub const HEADER_BYTES: usize = 4096;
pub const TOTAL_BYTES: usize = HEADER_BYTES + SLOT_COUNT * MAX_FRAME_BYTES;

/// Кадр считается устаревшим, если писатель молчит дольше этого.
pub const STALE_AFTER_MS: u64 = 1000;

const NO_SLOT: u32 = u32::MAX;

pub const fn nv12_size(width: u32, height: u32) -> usize {
    (width as usize) * (height as usize) * 3 / 2
}

#[repr(C)]
struct Slot {
    /// Нечётное значение — слот сейчас пишется.
    seq: AtomicU64,
    width: AtomicU32,
    height: AtomicU32,
}

#[repr(C)]
struct Header {
    magic: AtomicU32,
    version: AtomicU32,
    latest: AtomicU32,
    /// Сколько потоков виртуальной камеры сейчас запущено (выставляет DLL).
    consumers: AtomicU32,
    consumer_width: AtomicU32,
    consumer_height: AtomicU32,
    frame_counter: AtomicU64,
    /// Время последней записи, мс от UNIX-эпохи.
    written_at_ms: AtomicU64,
    slots: [Slot; SLOT_COUNT],
}

const _: () = assert!(size_of::<Header>() <= HEADER_BYTES);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameInfo {
    pub width: u32,
    pub height: u32,
    /// Монотонный номер кадра; по нему читатель понимает, что кадр новый.
    pub counter: u64,
}

/// Представление области общей памяти. Не владеет ею: владелец — `windows::Mapping`
/// или тестовый буфер.
pub struct SharedFrames {
    base: *mut u8,
}

unsafe impl Send for SharedFrames {}
unsafe impl Sync for SharedFrames {}

impl SharedFrames {
    /// # Safety
    /// `base` должен указывать на выровненную по 8 область размером не меньше `TOTAL_BYTES`,
    /// живущую дольше возвращённого значения.
    pub unsafe fn from_raw(base: *mut u8) -> Self {
        let this = Self { base };
        let h = this.header();
        if h.magic.load(Ordering::Acquire) != MAGIC {
            // Новое отображение заполнено нулями; инициализируем заголовок.
            h.latest.store(NO_SLOT, Ordering::Relaxed);
            h.version.store(VERSION, Ordering::Relaxed);
            h.magic.store(MAGIC, Ordering::Release);
        }
        this
    }

    fn header(&self) -> &Header {
        unsafe { &*(self.base as *const Header) }
    }

    fn slot_data(&self, slot: usize) -> *mut u8 {
        unsafe { self.base.add(HEADER_BYTES + slot * MAX_FRAME_BYTES) }
    }

    pub fn is_compatible(&self) -> bool {
        let h = self.header();
        h.magic.load(Ordering::Acquire) == MAGIC && h.version.load(Ordering::Acquire) == VERSION
    }

    /// Записывает кадр NV12: `fill` получает буфер ровно `nv12_size(width, height)` байт.
    pub fn write(&self, width: u32, height: u32, fill: impl FnOnce(&mut [u8])) {
        assert!(width <= MAX_WIDTH && height <= MAX_HEIGHT, "кадр больше максимального");
        let h = self.header();
        let latest = h.latest.load(Ordering::Acquire);
        let index = if latest as usize >= SLOT_COUNT { 0 } else { (latest as usize + 1) % SLOT_COUNT };
        let slot = &h.slots[index];

        let seq = slot.seq.load(Ordering::Relaxed) & !1;
        slot.seq.store(seq + 1, Ordering::Relaxed);
        fence(Ordering::Release);

        slot.width.store(width, Ordering::Relaxed);
        slot.height.store(height, Ordering::Relaxed);
        let data = unsafe { std::slice::from_raw_parts_mut(self.slot_data(index), nv12_size(width, height)) };
        fill(data);

        slot.seq.store(seq + 2, Ordering::Release);
        h.latest.store(index as u32, Ordering::Release);
        h.frame_counter.fetch_add(1, Ordering::AcqRel);
        h.written_at_ms.store(now_ms(), Ordering::Release);
    }

    /// Копирует самый свежий кадр в `dst`, если он новее `after` и не устарел.
    pub fn read_latest(&self, after: u64, dst: &mut Vec<u8>) -> Option<FrameInfo> {
        let h = self.header();
        let counter = h.frame_counter.load(Ordering::Acquire);
        if counter == after || self.is_stale() {
            return None;
        }
        let index = h.latest.load(Ordering::Acquire) as usize;
        if index >= SLOT_COUNT {
            return None;
        }
        let slot = &h.slots[index];
        let seq = slot.seq.load(Ordering::Acquire);
        if seq & 1 == 1 {
            return None;
        }
        let width = slot.width.load(Ordering::Relaxed);
        let height = slot.height.load(Ordering::Relaxed);
        if width == 0 || height == 0 || width > MAX_WIDTH || height > MAX_HEIGHT {
            return None;
        }
        let size = nv12_size(width, height);
        dst.resize(size, 0);
        unsafe { std::ptr::copy_nonoverlapping(self.slot_data(index), dst.as_mut_ptr(), size) };
        fence(Ordering::Acquire);
        if slot.seq.load(Ordering::Relaxed) != seq {
            return None;
        }
        Some(FrameInfo { width, height, counter })
    }

    /// Писатель давно не присылал кадров (телефон отключён или приложение закрыто).
    pub fn is_stale(&self) -> bool {
        let written = self.header().written_at_ms.load(Ordering::Acquire);
        written == 0 || now_ms().saturating_sub(written) > STALE_AFTER_MS
    }

    /// Вызывается DLL при запуске/остановке потока камеры.
    pub fn set_consumer(&self, active: bool, width: u32, height: u32) {
        let h = self.header();
        if active {
            h.consumer_width.store(width, Ordering::Relaxed);
            h.consumer_height.store(height, Ordering::Relaxed);
            h.consumers.fetch_add(1, Ordering::AcqRel);
        } else {
            let _ = h.consumers.fetch_update(Ordering::AcqRel, Ordering::Acquire, |c| c.checked_sub(1));
        }
    }

    /// Сколько приложений сейчас получают видео и в каком разрешении.
    pub fn consumer(&self) -> Option<(u32, u32)> {
        let h = self.header();
        (h.consumers.load(Ordering::Acquire) > 0)
            .then(|| (h.consumer_width.load(Ordering::Relaxed), h.consumer_height.load(Ordering::Relaxed)))
    }
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

#[cfg(windows)]
pub mod windows {
    use super::{SharedFrames, TOTAL_BYTES};
    use ::windows::Win32::Foundation::{CloseHandle, HANDLE, HLOCAL, INVALID_HANDLE_VALUE, LocalFree};
    use ::windows::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use ::windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
    use ::windows::Win32::System::Memory::{
        CreateFileMappingW, FILE_MAP_ALL_ACCESS, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile, OpenFileMappingW,
        PAGE_READWRITE, UnmapViewOfFile,
    };
    use ::windows::core::{Result, w};

    /// Глобальное имя: DLL камеры работает в сессии 0 (служба), приложение — в сессии пользователя.
    const NAME: ::windows::core::PCWSTR = w!("Global\\EscanorVirtualCamera");

    /// Доступ: система, LocalService (Frame Server), администраторы и вошедшие пользователи.
    /// Метка Low integrity, чтобы процесс с обычным уровнем мог писать в объект службы.
    const SDDL: ::windows::core::PCWSTR = w!("D:(A;;GA;;;SY)(A;;GA;;;LS)(A;;GA;;;BA)(A;;GA;;;AU)S:(ML;;NW;;;LW)");

    pub struct Mapping {
        handle: HANDLE,
        view: MEMORY_MAPPED_VIEW_ADDRESS,
        frames: SharedFrames,
    }

    unsafe impl Send for Mapping {}
    unsafe impl Sync for Mapping {}

    impl Mapping {
        /// Создаёт отображение, а если прав на создание в Global нет — открывает существующее.
        pub fn create_or_open() -> Result<Self> {
            match create() {
                Ok(handle) => Self::map(handle),
                Err(_) => Self::open(),
            }
        }

        /// Открывает отображение, созданное другим процессом.
        pub fn open() -> Result<Self> {
            let handle = unsafe { OpenFileMappingW(FILE_MAP_ALL_ACCESS.0, false, NAME)? };
            Self::map(handle)
        }

        fn map(handle: HANDLE) -> Result<Self> {
            let view = unsafe { MapViewOfFile(handle, FILE_MAP_ALL_ACCESS, 0, 0, TOTAL_BYTES) };
            if view.Value.is_null() {
                let err = ::windows::core::Error::from_thread();
                unsafe { CloseHandle(handle).ok() };
                return Err(err);
            }
            let frames = unsafe { SharedFrames::from_raw(view.Value as *mut u8) };
            Ok(Self { handle, view, frames })
        }

        pub fn frames(&self) -> &SharedFrames {
            &self.frames
        }
    }

    impl Drop for Mapping {
        fn drop(&mut self) {
            unsafe {
                UnmapViewOfFile(self.view).ok();
                CloseHandle(self.handle).ok();
            }
        }
    }

    fn create() -> Result<HANDLE> {
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(SDDL, SDDL_REVISION_1, &mut descriptor, None)? };
        let attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor.0,
            bInheritHandle: false.into(),
        };
        let size = TOTAL_BYTES as u64;
        let result = unsafe {
            CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                Some(&attributes),
                PAGE_READWRITE,
                (size >> 32) as u32,
                size as u32,
                NAME,
            )
        };
        unsafe { LocalFree(Some(HLOCAL(descriptor.0))) };
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn region() -> Vec<u64> {
        vec![0u64; TOTAL_BYTES / 8 + 1]
    }

    #[test]
    fn write_then_read() {
        let mut mem = region();
        let frames = unsafe { SharedFrames::from_raw(mem.as_mut_ptr() as *mut u8) };
        let mut dst = Vec::new();
        assert!(frames.read_latest(0, &mut dst).is_none());

        frames.write(4, 2, |buf| buf.copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]));
        let info = frames.read_latest(0, &mut dst).unwrap();
        assert_eq!((info.width, info.height, info.counter), (4, 2, 1));
        assert_eq!(dst, [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]);
        // Тот же кадр повторно не отдаётся.
        assert!(frames.read_latest(info.counter, &mut dst).is_none());

        frames.write(2, 2, |buf| buf.fill(7));
        let info = frames.read_latest(info.counter, &mut dst).unwrap();
        assert_eq!((info.width, info.height, info.counter), (2, 2, 2));
        assert_eq!(dst, [7; 6]);
    }

    #[test]
    fn finds_vcam_revision() {
        let dll = b"MZ...ESCANOR-VCAM-REVISION:0123abcd;...".to_vec();
        assert_eq!(vcam_revision(&dll), Some(&b"0123abcd"[..]));
        assert_eq!(vcam_revision(b"MZ without marker"), None);
        assert_eq!(vcam_revision(b"ESCANOR-VCAM-REVISION:no-end"), None);
    }

    #[test]
    fn consumer_counter() {
        let mut mem = region();
        let frames = unsafe { SharedFrames::from_raw(mem.as_mut_ptr() as *mut u8) };
        assert_eq!(frames.consumer(), None);
        frames.set_consumer(true, 1280, 720);
        assert_eq!(frames.consumer(), Some((1280, 720)));
        frames.set_consumer(false, 0, 0);
        frames.set_consumer(false, 0, 0);
        assert_eq!(frames.consumer(), None);
    }
}
