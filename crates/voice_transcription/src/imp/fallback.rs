use std::path::Path;

use crate::{Error, Result};

pub struct SystemSpeechRecognizer;

impl SystemSpeechRecognizer {
    pub fn new() -> Result<Self> {
        Err(anyhow::anyhow!("system speech recognition is not available on this platform").into())
    }

    pub fn transcribe_wav_bytes(&self, _wav_bytes: &[u8]) -> Result<String> {
        Err(anyhow::anyhow!("system speech recognition is not available on this platform").into())
    }

    pub fn transcribe_wav_file(&self, _path: impl AsRef<Path>) -> Result<String> {
        Err(anyhow::anyhow!("system speech recognition is not available on this platform").into())
    }
}
