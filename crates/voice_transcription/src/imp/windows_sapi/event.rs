use std::mem::MaybeUninit;

use windows::Win32::Media::Speech::{
    ISpEventSource, ISpRecoResult, SPEI_END_INPUT_STREAM, SPEI_END_SR_STREAM,
    SPEI_FALSE_RECOGNITION, SPEI_RECOGNITION, SPET_LPARAM_IS_OBJECT, SPET_LPARAM_IS_UNDEFINED,
    SPEVENT, SPEVENTENUM, SPEVENTLPARAMTYPE,
};
use windows_core::{IUnknown, Interface};

use super::sapi;
use crate::Result;

pub enum Event {
    Recognition(ISpRecoResult),
    FalseRecognition,
    EndStream,
    Other,
}

impl Event {
    fn from_sapi(sapi_event: SPEVENT) -> Result<Self> {
        let event_id = SPEVENTENUM(sapi_event._bitfield & 0xffff);
        let lparam_type = SPEVENTLPARAMTYPE(sapi_event._bitfield >> 16);

        if lparam_type == SPET_LPARAM_IS_OBJECT {
            let intf = unsafe { IUnknown::from_raw(sapi_event.lParam.0 as _) };
            if event_id == SPEI_RECOGNITION {
                Ok(Self::Recognition(intf.cast()?))
            } else if event_id == SPEI_FALSE_RECOGNITION {
                Ok(Self::FalseRecognition)
            } else {
                Ok(Self::Other)
            }
        } else if lparam_type == SPET_LPARAM_IS_UNDEFINED
            && (event_id == SPEI_END_INPUT_STREAM || event_id == SPEI_END_SR_STREAM)
        {
            Ok(Self::EndStream)
        } else {
            Ok(Self::Other)
        }
    }
}

pub struct EventSource {
    intf: ISpEventSource,
}

impl EventSource {
    pub fn new(intf: ISpEventSource) -> Self {
        Self { intf }
    }

    pub fn next_event(&self) -> Result<Option<Event>> {
        let mut event = MaybeUninit::<SPEVENT>::uninit();
        let mut fetched = 0;
        unsafe {
            sapi(
                "ISpEventSource::GetEvents",
                self.intf.GetEvents(1, event.as_mut_ptr(), &mut fetched),
            )?;
        }

        if fetched == 0 {
            Ok(None)
        } else {
            Ok(Some(Event::from_sapi(unsafe { event.assume_init() })?))
        }
    }
}
