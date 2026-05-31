//! 语音转录后端。
//!
//! 对外暴露统一的 [`SystemSpeechRecognizer`] 类型，内部按 `target_os` 分发到平台实现：
//! - Windows: SAPI / WinRT (`imp/windows.rs`, `imp/windows_realtime.rs`)
//! - macOS: SFSpeechRecognizer (后续)
//! - Linux/其他: 降级无操作 (`imp/fallback.rs`)

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[cfg(target_os = "windows")]
    #[error("Windows speech recognition error: {0}")]
    Windows(#[from] windows_core::Error),

    #[cfg(target_os = "windows")]
    #[error("Windows speech recognition operation `{operation}` failed: {source}")]
    WindowsOperation {
        operation: &'static str,
        #[source]
        source: windows_core::Error,
    },

    #[error("Windows speech recognition privacy is disabled. Enable speech recognition in Windows Settings > Privacy & security > Speech.")]
    SpeechPrivacyPolicyNotAccepted,

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("unsupported WAV format: {0}")]
    UnsupportedWavFormat(String),

    #[cfg(target_os = "windows")]
    #[error("Windows SAPI returned invalid UTF-16 text: {0}")]
    Utf16(#[from] std::string::FromUtf16Error),

    #[error("speech recognition timed out")]
    Timeout,

    #[error("speech recognition returned no text")]
    EmptyRecognition,

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl Error {
    #[cfg(target_os = "windows")]
    pub(crate) fn from_windows_operation(
        operation: &'static str,
        source: windows_core::Error,
    ) -> Self {
        const SPEECH_PRIVACY_POLICY_NOT_ACCEPTED: windows_core::HRESULT =
            windows_core::HRESULT(0x80045509u32 as i32);

        if source.code() == SPEECH_PRIVACY_POLICY_NOT_ACCEPTED {
            Self::SpeechPrivacyPolicyNotAccepted
        } else {
            Self::WindowsOperation { operation, source }
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

mod imp;
pub use imp::SystemSpeechRecognizer;
pub use imp::realtime::{RealtimeSpeechEvent, RealtimeSpeechRecognizer, RealtimeSpeechSession};
