use std::path::Path;
use std::time::Duration;

use tempfile::NamedTempFile;

use super::windows_sapi::{AudioFormat, Recognizer};
use crate::{Error, Result};

pub struct SystemSpeechRecognizer {
    timeout: Duration,
}

impl SystemSpeechRecognizer {
    pub fn new() -> Result<Self> {
        Ok(Self {
            timeout: Duration::from_secs(30),
        })
    }

    pub fn transcribe_wav_bytes(&self, wav_bytes: &[u8]) -> Result<String> {
        let mut wav_file = NamedTempFile::new()?;
        std::io::Write::write_all(&mut wav_file, wav_bytes)?;
        self.transcribe_wav_file(wav_file.path())
    }

    pub fn transcribe_wav_file(&self, path: impl AsRef<Path>) -> Result<String> {
        let text = super::windows_sapi::with_initialized_com(|| {
            let mut recognizer = Recognizer::new()?;
            let audio_format = AudioFormat::from_wav_file(path.as_ref())?;
            recognizer.set_input_from_wav_file(path.as_ref(), &audio_format)?;
            recognizer.recognize_dictation(self.timeout)
        })?;

        if text.trim().is_empty() {
            return Err(Error::EmptyRecognition);
        }

        Ok(text)
    }
}
