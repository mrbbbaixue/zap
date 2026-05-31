use std::path::Path;
use std::time::{Duration, Instant};

use windows::Win32::Media::Speech::{
    ISpRecoContext, ISpRecoGrammar, ISpRecognizer, SPCS_ENABLED, SPLO_STATIC, SPRS_ACTIVE,
    SPRS_INACTIVE, SPRST_ACTIVE, SpInprocRecognizer,
};
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance};
use windows_core::{IUnknown, Interface, PCWSTR};

use super::audio::{AudioFormat, AudioStream};
use super::event::{Event, EventSource};
use super::phrase::text_from_result;
use super::sapi;
use crate::{Error, Result};

pub struct Recognizer {
    intf: ISpRecognizer,
    input_stream: Option<AudioStream>,
}

impl Recognizer {
    pub fn new() -> Result<Self> {
        let intf: ISpRecognizer = sapi("CoCreateInstance(SpInprocRecognizer)", unsafe {
            CoCreateInstance(&SpInprocRecognizer, None, CLSCTX_ALL)
        })?;
        Ok(Self {
            intf,
            input_stream: None,
        })
    }

    pub fn set_input_from_wav_file(&mut self, path: &Path, format: &AudioFormat) -> Result<()> {
        let stream = AudioStream::open_file(path, format)?;
        let input: IUnknown = stream.to_sapi().cast()?;
        unsafe { sapi("ISpRecognizer::SetInput", self.intf.SetInput(&input, false))? };
        self.input_stream = Some(stream);
        Ok(())
    }

    pub fn recognize_dictation(&self, timeout: Duration) -> Result<String> {
        let context = unsafe {
            sapi(
                "ISpRecognizer::CreateRecoContext",
                self.intf.CreateRecoContext(),
            )?
        };
        unsafe {
            sapi(
                "ISpRecoContext::SetNotifyWin32Event",
                context.SetNotifyWin32Event(),
            )?
        };

        let grammar = DictationGrammar::new(&context)?;
        grammar.set_enabled(true)?;

        unsafe {
            sapi(
                "ISpRecoContext::SetContextState",
                context.SetContextState(SPCS_ENABLED),
            )?;
            sapi(
                "ISpRecognizer::SetRecoState",
                self.intf.SetRecoState(SPRST_ACTIVE),
            )?;
        }

        let event_source = EventSource::new(context.cast()?);
        let text = wait_for_recognition(&context, &event_source, timeout)?;
        grammar.set_enabled(false)?;
        Ok(text)
    }
}

struct DictationGrammar {
    intf: ISpRecoGrammar,
}

impl DictationGrammar {
    fn new(context: &ISpRecoContext) -> Result<Self> {
        let intf = unsafe { sapi("ISpRecoContext::CreateGrammar", context.CreateGrammar(0))? };
        unsafe {
            sapi(
                "ISpRecoGrammar::LoadDictation",
                intf.LoadDictation(PCWSTR::null(), SPLO_STATIC),
            )?
        };
        Ok(Self { intf })
    }

    fn set_enabled(&self, enabled: bool) -> Result<()> {
        let state = if enabled { SPRS_ACTIVE } else { SPRS_INACTIVE };
        unsafe {
            sapi(
                "ISpRecoGrammar::SetDictationState",
                self.intf.SetDictationState(state),
            )?
        };
        Ok(())
    }
}

fn wait_for_recognition(
    context: &ISpRecoContext,
    event_source: &EventSource,
    timeout: Duration,
) -> Result<String> {
    let deadline = Instant::now() + timeout;
    loop {
        match drain_events(event_source)? {
            DrainResult::Recognition(text) => return Ok(text),
            DrainResult::EndStream => return Ok(String::new()),
            DrainResult::Pending => {}
        }

        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or(Error::Timeout)?;
        let timeout_ms = remaining.as_millis().try_into().unwrap_or(u32::MAX - 1);
        unsafe {
            sapi(
                "ISpRecoContext::WaitForNotifyEvent",
                context.WaitForNotifyEvent(timeout_ms),
            )?
        };
    }
}

enum DrainResult {
    Recognition(String),
    EndStream,
    Pending,
}

fn drain_events(event_source: &EventSource) -> Result<DrainResult> {
    let mut end_stream = false;
    while let Some(event) = event_source.next_event()? {
        match event {
            Event::Recognition(result) => {
                return Ok(DrainResult::Recognition(text_from_result(&result)?));
            }
            Event::EndStream => end_stream = true,
            Event::FalseRecognition | Event::Other => {}
        }
    }

    if end_stream {
        Ok(DrainResult::EndStream)
    } else {
        Ok(DrainResult::Pending)
    }
}
