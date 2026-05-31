use async_channel::{Receiver, Sender, TrySendError};
use futures::FutureExt as _;
use thiserror::Error;
use voice_transcription::{
    Error as TranscriptionError, RealtimeSpeechEvent, RealtimeSpeechRecognizer,
};
use warpui::event::KeyState;
use warpui::platform::MicrophoneAccessState;
use warpui::{Entity, ModelContext, SingletonEntity};

pub enum VoiceSessionCommand {
    Stop,
    Cancel,
}

#[derive(Default)]
pub enum VoiceInputState {
    #[default]
    Idle,
    Listening {
        enabled_from: VoiceInputToggledFrom,
        session_start: instant::Instant,
        command_tx: Sender<VoiceSessionCommand>,
    },
}

#[derive(Debug, Clone)]
pub enum VoiceInputToggledFrom {
    Button,
    Key { state: KeyState },
}

#[derive(Debug, Clone)]
pub enum VoiceSessionEvent {
    Hypothesis { text: String },
    Final { text: String },
    Completed { session_duration_ms: u64 },
    Canceled { session_duration_ms: Option<u64> },
    Error(VoiceSessionError),
}

#[derive(Debug, Clone)]
pub enum VoiceSessionError {
    SpeechPrivacyPolicyNotAccepted,
    Other(String),
}

impl std::fmt::Display for VoiceSessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SpeechPrivacyPolicyNotAccepted => write!(
                f,
                "Windows speech recognition privacy is disabled. Enable speech recognition in Windows Settings > Privacy & security > Speech."
            ),
            Self::Other(message) => f.write_str(message),
        }
    }
}

impl From<&TranscriptionError> for VoiceSessionError {
    fn from(error: &TranscriptionError) -> Self {
        match error {
            TranscriptionError::SpeechPrivacyPolicyNotAccepted => {
                Self::SpeechPrivacyPolicyNotAccepted
            }
            #[cfg(target_os = "windows")]
            TranscriptionError::Windows(_) | TranscriptionError::WindowsOperation { .. } => {
                Self::Other(error.to_string())
            }
            TranscriptionError::Io(_)
            | TranscriptionError::UnsupportedWavFormat(_)
            | TranscriptionError::Timeout
            | TranscriptionError::EmptyRecognition
            | TranscriptionError::Other(_) => Self::Other(error.to_string()),
            #[cfg(target_os = "windows")]
            TranscriptionError::Utf16(_) => Self::Other(error.to_string()),
        }
    }
}

#[derive(Debug, Error)]
pub enum StartListeningError {
    #[error("Voice input is already running")]
    AlreadyRunning,
    #[error("Microphone access denied")]
    AccessDenied,
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

pub struct VoiceInput {
    state: VoiceInputState,
    pub should_suppress_new_feature_popup: bool,
    recognizer: RealtimeSpeechRecognizer,
}

impl VoiceInput {
    pub fn new(_ctx: &mut ModelContext<Self>) -> Self {
        Self {
            state: VoiceInputState::Idle,
            should_suppress_new_feature_popup: false,
            recognizer: RealtimeSpeechRecognizer::new()
                .expect("failed to create real-time speech recognizer"),
        }
    }

    pub fn is_listening(&self) -> bool {
        matches!(self.state, VoiceInputState::Listening { .. })
    }

    pub fn is_active(&self) -> bool {
        self.is_listening()
    }

    pub fn state(&self) -> &VoiceInputState {
        &self.state
    }

    pub fn start_listening(
        &mut self,
        source: VoiceInputToggledFrom,
        ctx: &mut ModelContext<Self>,
    ) -> Result<Receiver<VoiceSessionEvent>, StartListeningError> {
        if self.is_listening() {
            return Err(StartListeningError::AlreadyRunning);
        }

        if matches!(
            ctx.microphone_access_state(),
            MicrophoneAccessState::Denied | MicrophoneAccessState::Restricted
        ) {
            return Err(StartListeningError::AccessDenied);
        }

        let (event_tx, event_rx) = async_channel::unbounded();
        let (command_tx, command_rx) = async_channel::bounded(1);
        let session_start = instant::Instant::now();

        self.state = VoiceInputState::Listening {
            enabled_from: source,
            session_start,
            command_tx,
        };

        ctx.spawn(
            Self::run_session(self.recognizer.clone(), event_tx, command_rx, session_start),
            |me, result, ctx| {
                if let Err(error) = result {
                    log::error!("Voice input session failed: {error:?}");
                }
                me.state = VoiceInputState::Idle;
                ctx.notify();
            },
        );

        Ok(event_rx)
    }

    pub fn stop_listening(&self) -> Result<(), anyhow::Error> {
        let Some(command_tx) = self.command_tx() else {
            return Ok(());
        };

        match command_tx.try_send(VoiceSessionCommand::Stop) {
            Ok(()) | Err(TrySendError::Full(_)) | Err(TrySendError::Closed(_)) => Ok(()),
        }
    }

    pub fn abort_listening(&self) {
        if let Some(command_tx) = self.command_tx() {
            let _ = command_tx.try_send(VoiceSessionCommand::Cancel);
        }
    }

    fn command_tx(&self) -> Option<&Sender<VoiceSessionCommand>> {
        match &self.state {
            VoiceInputState::Listening { command_tx, .. } => Some(command_tx),
            VoiceInputState::Idle => None,
        }
    }

    async fn run_session(
        recognizer: RealtimeSpeechRecognizer,
        event_tx: Sender<VoiceSessionEvent>,
        command_rx: Receiver<VoiceSessionCommand>,
        session_start: instant::Instant,
    ) -> Result<(), anyhow::Error> {
        let session = match recognizer.start_session().await {
            Ok(session) => session,
            Err(error) => {
                Self::send_event(&event_tx, VoiceSessionEvent::Error((&error).into())).await;
                return Err(error.into());
            }
        };
        let events = session.events();

        loop {
            futures::select! {
                command = command_rx.recv().fuse() => {
                    match command {
                        Ok(VoiceSessionCommand::Stop) => {
                            session.stop().await?;
                            if !Self::forward_pending_events(&events, &event_tx, session_start).await {
                                Self::send_completed(&event_tx, session_start).await;
                            }
                            break;
                        }
                        Ok(VoiceSessionCommand::Cancel) | Err(_) => {
                            session.cancel().await?;
                            if !Self::forward_pending_events(&events, &event_tx, session_start).await {
                                Self::send_canceled(&event_tx, session_start).await;
                            }
                            break;
                        }
                    }
                }
                event = events.recv().fuse() => {
                    match event {
                        Ok(event) => {
                            if Self::forward_realtime_event(event, &event_tx, session_start).await {
                                break;
                            }
                        }
                        Err(_) => {
                            Self::send_completed(&event_tx, session_start).await;
                            break;
                        }
                    }
                }
            }
        }

        drop(session);
        Ok(())
    }

    async fn forward_pending_events(
        events: &Receiver<RealtimeSpeechEvent>,
        event_tx: &Sender<VoiceSessionEvent>,
        session_start: instant::Instant,
    ) -> bool {
        let mut saw_terminal_event = false;

        while let Ok(event) = events.try_recv() {
            if Self::forward_realtime_event(event, event_tx, session_start).await {
                saw_terminal_event = true;
                break;
            }
        }

        saw_terminal_event
    }

    async fn forward_realtime_event(
        event: RealtimeSpeechEvent,
        event_tx: &Sender<VoiceSessionEvent>,
        session_start: instant::Instant,
    ) -> bool {
        match event {
            RealtimeSpeechEvent::Hypothesis { text } => {
                Self::send_event(event_tx, VoiceSessionEvent::Hypothesis { text }).await;
                false
            }
            RealtimeSpeechEvent::Final { text } => {
                Self::send_event(event_tx, VoiceSessionEvent::Final { text }).await;
                false
            }
            RealtimeSpeechEvent::Completed => {
                Self::send_completed(event_tx, session_start).await;
                true
            }
            RealtimeSpeechEvent::Canceled => {
                Self::send_canceled(event_tx, session_start).await;
                true
            }
            RealtimeSpeechEvent::Error(error) => {
                Self::send_event(
                    event_tx,
                    VoiceSessionEvent::Error(VoiceSessionError::Other(error)),
                )
                .await;
                true
            }
        }
    }

    async fn send_completed(event_tx: &Sender<VoiceSessionEvent>, session_start: instant::Instant) {
        let session_duration_ms = session_start.elapsed().as_millis() as u64;
        Self::send_event(
            event_tx,
            VoiceSessionEvent::Completed {
                session_duration_ms,
            },
        )
        .await;
    }

    async fn send_canceled(event_tx: &Sender<VoiceSessionEvent>, session_start: instant::Instant) {
        let session_duration_ms = Some(session_start.elapsed().as_millis() as u64);
        Self::send_event(
            event_tx,
            VoiceSessionEvent::Canceled {
                session_duration_ms,
            },
        )
        .await;
    }

    async fn send_event(event_tx: &Sender<VoiceSessionEvent>, event: VoiceSessionEvent) {
        let _ = event_tx.send(event).await;
    }
}

impl Entity for VoiceInput {
    type Event = ();
}

impl SingletonEntity for VoiceInput {}
