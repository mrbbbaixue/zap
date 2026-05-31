use windows::Win32::Foundation::RPC_E_CHANGED_MODE;
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};

use crate::Result;

pub fn with_initialized_com<T>(f: impl FnOnce() -> Result<T>) -> Result<T> {
    let init_result = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    let should_uninitialize = init_result.is_ok();

    if init_result.is_err() && init_result != RPC_E_CHANGED_MODE {
        init_result.ok()?;
    }

    let result = f();

    if should_uninitialize {
        unsafe { CoUninitialize() };
    }

    result
}
