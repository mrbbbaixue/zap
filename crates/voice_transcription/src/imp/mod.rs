#[cfg(target_os = "windows")]
mod windows;
#[cfg(not(target_os = "windows"))]
mod fallback;

#[cfg(target_os = "windows")]
mod windows_sapi;

#[cfg(target_os = "windows")]
pub use windows::SystemSpeechRecognizer;
#[cfg(not(target_os = "windows"))]
pub use fallback::SystemSpeechRecognizer;

// Realtime speech recognition module
pub mod realtime {
    #[cfg(target_os = "windows")]
    pub use super::windows_realtime::{RealtimeSpeechEvent, RealtimeSpeechRecognizer, RealtimeSpeechSession};

    #[cfg(not(target_os = "windows"))]
    pub use super::realtime_fallback::{RealtimeSpeechEvent, RealtimeSpeechRecognizer, RealtimeSpeechSession};
}

#[cfg(target_os = "windows")]
mod windows_realtime;

#[cfg(not(target_os = "windows"))]
mod realtime_fallback {
    use crate::Result;

    #[derive(Debug, Clone)]
    pub enum RealtimeSpeechEvent {
        Hypothesis { text: String },
        Final { text: String },
        Completed,
        Canceled,
        Error(String),
    }

    #[derive(Clone)]
    pub struct RealtimeSpeechRecognizer;

    impl RealtimeSpeechRecognizer {
        pub fn new() -> Result<Self> {
            Ok(Self)
        }

        pub async fn start_session(&self) -> Result<RealtimeSpeechSession> {
            Err(anyhow::anyhow!(
                "real-time system speech recognition is only available on Windows"
            )
            .into())
        }
    }

    pub struct RealtimeSpeechSession;

    impl RealtimeSpeechSession {
        pub fn events(&self) -> async_channel::Receiver<RealtimeSpeechEvent> {
            let (_tx, rx) = async_channel::unbounded();
            rx
        }

        pub async fn stop(&self) -> Result<()> {
            Ok(())
        }

        pub async fn cancel(&self) -> Result<()> {
            Ok(())
        }
    }
}
