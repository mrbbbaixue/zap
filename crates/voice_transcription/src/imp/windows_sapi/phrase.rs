use windows::Win32::Media::Speech::{ISpRecoResult, SPPR_ALL_ELEMENTS};
use windows::Win32::System::Com::CoTaskMemFree;
use windows_core::PWSTR;

use super::sapi;
use crate::Result;

pub fn text_from_result(result: &ISpRecoResult) -> Result<String> {
    let mut text = PWSTR::null();
    unsafe {
        sapi(
            "ISpRecoResult::GetText",
            result.GetText(
                SPPR_ALL_ELEMENTS.0 as u32,
                SPPR_ALL_ELEMENTS.0 as u32,
                true,
                &mut text,
                None,
            ),
        )?;
    }

    if text.is_null() {
        return Ok(String::new());
    }

    let rust_text = unsafe { text.to_string() };
    unsafe { CoTaskMemFree(Some(text.as_ptr().cast())) };
    Ok(rust_text?)
}
