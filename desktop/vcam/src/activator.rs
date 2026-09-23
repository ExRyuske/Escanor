//! IMFActivate — то, что COM-фабрика отдаёт службе Frame Server.
//! Служба читает его атрибуты и вызывает ActivateObject, чтобы получить медиаисточник.

use crate::dll::CLSID;
use crate::log::{Live, vlog};
use crate::source::MediaSource;
use std::ffi::c_void;
use std::sync::Mutex;
use windows::Win32::Media::MediaFoundation::{
    IMFActivate, IMFActivate_Impl, IMFAttributes, IMFAttributes_Impl, IMFMediaSourceEx, MF_ATTRIBUTE_TYPE,
    MF_ATTRIBUTES_MATCH_TYPE, MF_VIRTUALCAMERA_PROVIDE_ASSOCIATED_CAMERA_SOURCES, MFCreateAttributes,
    MFT_TRANSFORM_CLSID_Attribute,
};
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::core::{BOOL, GUID, IUnknown, Interface, PCWSTR, PWSTR, Ref, Result, implement};

#[implement(IMFActivate)]
pub struct Activator {
    attributes: IMFAttributes,
    source: Mutex<Option<IMFMediaSourceEx>>,
    _live: Live,
}

impl Activator {
    pub fn create() -> Result<IUnknown> {
        let mut attributes = None;
        unsafe { MFCreateAttributes(&mut attributes, 4)? };
        let attributes = attributes.unwrap();
        unsafe {
            attributes.SetUINT32(&MF_VIRTUALCAMERA_PROVIDE_ASSOCIATED_CAMERA_SOURCES, 1)?;
            attributes.SetGUID(&MFT_TRANSFORM_CLSID_Attribute, &CLSID)?;
        }
        Ok(Activator { attributes, source: Mutex::new(None), _live: Live::new() }.into())
    }
}

impl IMFActivate_Impl for Activator_Impl {
    fn ActivateObject(&self, riid: *const GUID, ppv: *mut *mut c_void) -> Result<()> {
        let mut guard = self.source.lock().unwrap();
        let source = match &*guard {
            Some(source) => source.clone(),
            None => {
                let source = MediaSource::create(&self.attributes).inspect_err(|e| vlog!("не создан источник: {e}"))?;
                vlog!("источник создан");
                *guard = Some(source.clone());
                source
            }
        };
        unsafe { source.query(riid, ppv).ok() }
    }

    fn ShutdownObject(&self) -> Result<()> {
        if let Some(source) = self.source.lock().unwrap().take() {
            unsafe {
                let _ = source.Shutdown();
            }
        }
        Ok(())
    }

    fn DetachObject(&self) -> Result<()> {
        self.source.lock().unwrap().take();
        Ok(())
    }
}

/// Вызов метода внутреннего IMFAttributes напрямую через vtable.
macro_rules! forward {
    ($self:ident, $method:ident ( $($arg:expr),* )) => {
        unsafe { (Interface::vtable(&$self.attributes).$method)(Interface::as_raw(&$self.attributes), $($arg),*).ok() }
    };
}

fn raw<T: Interface>(r: Ref<T>) -> *mut c_void {
    r.as_ref().map_or(std::ptr::null_mut(), |i| i.as_raw())
}

impl IMFAttributes_Impl for Activator_Impl {
    fn GetItem(&self, key: *const GUID, value: *mut PROPVARIANT) -> Result<()> {
        forward!(self, GetItem(key, value))
    }
    fn GetItemType(&self, key: *const GUID) -> Result<MF_ATTRIBUTE_TYPE> {
        let mut t = MF_ATTRIBUTE_TYPE::default();
        forward!(self, GetItemType(key, &mut t))?;
        Ok(t)
    }
    fn CompareItem(&self, key: *const GUID, value: *const PROPVARIANT) -> Result<BOOL> {
        let mut r = BOOL::default();
        forward!(self, CompareItem(key, value, &mut r))?;
        Ok(r)
    }
    fn Compare(&self, theirs: Ref<IMFAttributes>, match_type: MF_ATTRIBUTES_MATCH_TYPE) -> Result<BOOL> {
        let mut r = BOOL::default();
        forward!(self, Compare(raw(theirs), match_type, &mut r))?;
        Ok(r)
    }
    fn GetUINT32(&self, key: *const GUID) -> Result<u32> {
        let mut v = 0;
        forward!(self, GetUINT32(key, &mut v))?;
        Ok(v)
    }
    fn GetUINT64(&self, key: *const GUID) -> Result<u64> {
        let mut v = 0;
        forward!(self, GetUINT64(key, &mut v))?;
        Ok(v)
    }
    fn GetDouble(&self, key: *const GUID) -> Result<f64> {
        let mut v = 0.0;
        forward!(self, GetDouble(key, &mut v))?;
        Ok(v)
    }
    fn GetGUID(&self, key: *const GUID) -> Result<GUID> {
        let mut v = GUID::zeroed();
        forward!(self, GetGUID(key, &mut v))?;
        Ok(v)
    }
    fn GetStringLength(&self, key: *const GUID) -> Result<u32> {
        let mut v = 0;
        forward!(self, GetStringLength(key, &mut v))?;
        Ok(v)
    }
    fn GetString(&self, key: *const GUID, value: PWSTR, size: u32, length: *mut u32) -> Result<()> {
        forward!(self, GetString(key, value, size, length))
    }
    fn GetAllocatedString(&self, key: *const GUID, value: *mut PWSTR, length: *mut u32) -> Result<()> {
        forward!(self, GetAllocatedString(key, value, length))
    }
    fn GetBlobSize(&self, key: *const GUID) -> Result<u32> {
        let mut v = 0;
        forward!(self, GetBlobSize(key, &mut v))?;
        Ok(v)
    }
    fn GetBlob(&self, key: *const GUID, buf: *mut u8, size: u32, blob_size: *mut u32) -> Result<()> {
        forward!(self, GetBlob(key, buf, size, blob_size))
    }
    fn GetAllocatedBlob(&self, key: *const GUID, buf: *mut *mut u8, size: *mut u32) -> Result<()> {
        forward!(self, GetAllocatedBlob(key, buf, size))
    }
    fn GetUnknown(&self, key: *const GUID, riid: *const GUID, ppv: *mut *mut c_void) -> Result<()> {
        forward!(self, GetUnknown(key, riid, ppv))
    }
    fn SetItem(&self, key: *const GUID, value: *const PROPVARIANT) -> Result<()> {
        forward!(self, SetItem(key, value))
    }
    fn DeleteItem(&self, key: *const GUID) -> Result<()> {
        forward!(self, DeleteItem(key))
    }
    fn DeleteAllItems(&self) -> Result<()> {
        forward!(self, DeleteAllItems())
    }
    fn SetUINT32(&self, key: *const GUID, value: u32) -> Result<()> {
        forward!(self, SetUINT32(key, value))
    }
    fn SetUINT64(&self, key: *const GUID, value: u64) -> Result<()> {
        forward!(self, SetUINT64(key, value))
    }
    fn SetDouble(&self, key: *const GUID, value: f64) -> Result<()> {
        forward!(self, SetDouble(key, value))
    }
    fn SetGUID(&self, key: *const GUID, value: *const GUID) -> Result<()> {
        forward!(self, SetGUID(key, value))
    }
    fn SetString(&self, key: *const GUID, value: &PCWSTR) -> Result<()> {
        forward!(self, SetString(key, *value))
    }
    fn SetBlob(&self, key: *const GUID, buf: *const u8, size: u32) -> Result<()> {
        forward!(self, SetBlob(key, buf, size))
    }
    fn SetUnknown(&self, key: *const GUID, unknown: Ref<IUnknown>) -> Result<()> {
        forward!(self, SetUnknown(key, raw(unknown)))
    }
    fn LockStore(&self) -> Result<()> {
        forward!(self, LockStore())
    }
    fn UnlockStore(&self) -> Result<()> {
        forward!(self, UnlockStore())
    }
    fn GetCount(&self) -> Result<u32> {
        let mut v = 0;
        forward!(self, GetCount(&mut v))?;
        Ok(v)
    }
    fn GetItemByIndex(&self, index: u32, key: *mut GUID, value: *mut PROPVARIANT) -> Result<()> {
        forward!(self, GetItemByIndex(index, key, value))
    }
    fn CopyAllItems(&self, dest: Ref<IMFAttributes>) -> Result<()> {
        forward!(self, CopyAllItems(raw(dest)))
    }
}
