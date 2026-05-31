use std::sync::Arc;

use async_trait::async_trait;
use voice_transcription::SystemSpeechRecognizer;
use warpui::{Entity, SingletonEntity};

#[derive(thiserror::Error, Debug)]
pub enum TranscribeError {
    #[error("Request failed due to lack of Voice quota.")]
    QuotaLimit,

    #[error("Zap is currently overloaded. Please try again later.")]
    ServerOverloaded,

    #[error("Internal error occurred at transport layer.")]
    Transport,

    #[error("Failed to deserialize JSON.")]
    Deserialization,

    /// Zap 已禁用语音转写(BYOP genai 协议无法承载音频)。
    #[error("Voice transcription is unavailable in Zap.")]
    Disabled,

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Interface for transcribing voice input.
#[cfg_attr(not(target_family = "wasm"), async_trait)]
#[cfg_attr(target_family = "wasm", async_trait(?Send))]
pub trait Transcriber: Send + Sync {
    /// Transcribe the given base64 encoded wav file into text.
    /// This is expected to be async and called off the main thread.
    async fn transcribe(&self, wav_base64: String) -> Result<String, TranscribeError>;
}

/// A voice transcriber that is enabled or disabled.
pub struct VoiceTranscriber {
    #[cfg_attr(not(feature = "voice_input"), allow(dead_code))]
    transcriber: Option<Arc<dyn Transcriber>>,
}

impl VoiceTranscriber {
    pub fn new(transcriber: Arc<dyn Transcriber>) -> Self {
        Self {
            transcriber: Some(transcriber),
        }
    }

    pub fn disabled() -> Self {
        Self { transcriber: None }
    }

    pub fn from_option(transcriber: Option<Arc<dyn Transcriber>>) -> Self {
        Self { transcriber }
    }

    pub fn transcriber(&self) -> Option<&Arc<dyn Transcriber>> {
        self.transcriber.as_ref()
    }
}

impl Entity for VoiceTranscriber {
    type Event = ();
}

impl SingletonEntity for VoiceTranscriber {}

/// 系统语音识别器 adapter，将 batch 接口适配到 `Transcriber` trait。
/// SAPI COM 对象不能跨线程，每次 transcribe 在 blocking 线程内新建识别器。
pub struct SystemSpeechRecognizerAdapter;

impl SystemSpeechRecognizerAdapter {
    pub fn new() -> Result<Self, voice_transcription::Error> {
        SystemSpeechRecognizer::new()?;
        Ok(Self)
    }
}

#[async_trait]
impl Transcriber for SystemSpeechRecognizerAdapter {
    async fn transcribe(&self, wav_base64: String) -> Result<String, TranscribeError> {
        use base64::Engine;
        let wav_bytes = base64::engine::general_purpose::STANDARD
            .decode(&wav_base64)
            .map_err(|e: base64::DecodeError| TranscribeError::Other(e.into()))?;

        let text = tokio::task::spawn_blocking(move || {
            let recognizer = SystemSpeechRecognizer::new()
                .map_err(|e: voice_transcription::Error| TranscribeError::Other(e.into()))?;
            recognizer
                .transcribe_wav_bytes(&wav_bytes)
                .map_err(|e: voice_transcription::Error| TranscribeError::Other(e.into()))
        })
        .await
        .map_err(|e: tokio::task::JoinError| TranscribeError::Other(e.into()))??;

        Ok(text)
    }
}
