//! Экспортируемые функции DLL: фабрика классов и саморегистрация (regsvr32).

use crate::activator::Activator;
use crate::log::{LIVE_OBJECTS, vlog};
use escanor_shm::{VCAM_CLSID, VCAM_CLSID_STR};
use std::ffi::c_void;
use windows::Win32::Foundation::{CLASS_E_CLASSNOTAVAILABLE, CLASS_E_NOAGGREGATION, E_POINTER, HMODULE, S_FALSE, S_OK};
use windows::Win32::System::Com::{IClassFactory, IClassFactory_Impl};
use windows::Win32::System::LibraryLoader::{
    GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT, GetModuleFileNameW,
    GetModuleHandleExW,
};
use windows::Win32::System::Registry::{
    HKEY, HKEY_LOCAL_MACHINE, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ, RegCloseKey, RegCreateKeyExW, RegDeleteTreeW,
    RegSetValueExW,
};
use windows::core::{BOOL, GUID, HRESULT, HSTRING, IUnknown, Interface, PCWSTR, Ref, Result, implement, w};

pub const CLSID: GUID = GUID::from_u128(VCAM_CLSID);

#[implement(IClassFactory)]
struct ClassFactory;

impl IClassFactory_Impl for ClassFactory_Impl {
    fn CreateInstance(&self, outer: Ref<IUnknown>, riid: *const GUID, ppv: *mut *mut c_void) -> Result<()> {
        if ppv.is_null() {
            return Err(E_POINTER.into());
        }
        unsafe { *ppv = std::ptr::null_mut() };
        if !outer.is_null() {
            return Err(CLASS_E_NOAGGREGATION.into());
        }
        let activator = Activator::create().inspect_err(|e| vlog!("не создан активатор: {e}"))?;
        unsafe { activator.query(riid, ppv).ok() }
    }

    fn LockServer(&self, _lock: BOOL) -> Result<()> {
        Ok(())
    }
}

#[unsafe(no_mangle)]
extern "system" fn DllGetClassObject(rclsid: *const GUID, riid: *const GUID, ppv: *mut *mut c_void) -> HRESULT {
    if ppv.is_null() || rclsid.is_null() {
        return E_POINTER;
    }
    unsafe { *ppv = std::ptr::null_mut() };
    if unsafe { *rclsid } != CLSID {
        return CLASS_E_CLASSNOTAVAILABLE;
    }
    vlog!("DLL загружена службой камеры");
    let factory: IClassFactory = ClassFactory.into();
    unsafe { factory.query(riid, ppv) }
}

/// Выгружаемся, когда живых объектов нет — иначе служба держит старую версию DLL до перезагрузки.
#[unsafe(no_mangle)]
extern "system" fn DllCanUnloadNow() -> HRESULT {
    if LIVE_OBJECTS.load(std::sync::atomic::Ordering::Acquire) == 0 { S_OK } else { S_FALSE }
}

#[unsafe(no_mangle)]
extern "system" fn DllRegisterServer() -> HRESULT {
    match register() {
        Ok(()) => S_OK,
        Err(e) => e.code(),
    }
}

#[unsafe(no_mangle)]
extern "system" fn DllUnregisterServer() -> HRESULT {
    let key = HSTRING::from(format!("SOFTWARE\\Classes\\CLSID\\{VCAM_CLSID_STR}"));
    let _ = unsafe { RegDeleteTreeW(HKEY_LOCAL_MACHINE, &key) };
    S_OK
}

/// Регистрация в HKLM: Frame Server работает как служба и не видит HKCU пользователя.
fn register() -> Result<()> {
    let path = module_path()?;
    let clsid_key = format!("SOFTWARE\\Classes\\CLSID\\{VCAM_CLSID_STR}");
    let key = create_key(&clsid_key)?;
    let result = set_string(key, PCWSTR::null(), "Escanor Virtual Camera");
    unsafe { RegCloseKey(key).ok()? };
    result?;

    let key = create_key(&format!("{clsid_key}\\InprocServer32"))?;
    let result = set_string(key, PCWSTR::null(), &path).and_then(|_| set_string(key, w!("ThreadingModel"), "Both"));
    unsafe { RegCloseKey(key).ok()? };
    result
}

fn create_key(path: &str) -> Result<HKEY> {
    let mut key = HKEY::default();
    unsafe {
        RegCreateKeyExW(
            HKEY_LOCAL_MACHINE,
            &HSTRING::from(path),
            None,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut key,
            None,
        )
        .ok()?;
    }
    Ok(key)
}

fn set_string(key: HKEY, name: PCWSTR, value: &str) -> Result<()> {
    let wide: Vec<u16> = value.encode_utf16().chain([0]).collect();
    let bytes = unsafe { std::slice::from_raw_parts(wide.as_ptr() as *const u8, wide.len() * 2) };
    unsafe { RegSetValueExW(key, name, None, REG_SZ, Some(bytes)).ok() }
}

fn module_path() -> Result<String> {
    let mut module = HMODULE::default();
    unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            PCWSTR(DllRegisterServer as *const () as *const u16),
            &mut module,
        )?;
    }
    let mut buf = [0u16; 1024];
    let len = unsafe { GetModuleFileNameW(Some(module), &mut buf) } as usize;
    Ok(String::from_utf16_lossy(&buf[..len]))
}
