//! WinRT 实时语音识别实现。
//!
//! 使用 Windows.Media.SpeechRecognition API 提供连续识别会话，
//! 支持 Hypothesis（中间结果）和 Final（最终结果）事件。

use crate::{Error, Result};
use async_channel::Receiver;
use windows::Media::SpeechRecognition::*;
use windows::Foundation::TypedEventHandler;

/// 实时语音识别事件。
#[derive(Debug, Clone)]
pub enum RealtimeSpeechEvent {
    /// 中间识别文本（说话过程中持续返回，可能被后续识别修正）。
    Hypothesis { text: String },
    /// 最终识别文本（停顿后确认的最终结果）。
    Final { text: String },
    /// 会话正常完成。
    Completed,
    /// 会话被取消。
    Canceled,
    /// 识别出错。
    Error(String),
}

/// 实时语音识别器工厂。
///
/// 轻量级结构体，用于创建新的实时识别会话。
#[derive(Clone)]
pub struct RealtimeSpeechRecognizer;

impl RealtimeSpeechRecognizer {
    /// 创建新的识别器实例。
    pub fn new() -> Result<Self> {
        Ok(Self)
    }

    /// 启动一个新的实时识别会话。
    ///
    /// 返回会话对象和事件接收器。会话必须显式停止或取消。
    pub async fn start_session(&self) -> Result<RealtimeSpeechSession> {
        RealtimeSpeechSession::new().await
    }
}

/// 实时语音识别会话。
///
/// 持有 WinRT SpeechRecognizer 对象，通过异步事件通道返回识别结果。
pub struct RealtimeSpeechSession {
    recognizer: SpeechRecognizer,
    event_rx: Receiver<RealtimeSpeechEvent>,
    hypothesis_token: i64,
    result_token: i64,
    completed_token: i64,
}

impl RealtimeSpeechSession {
    /// 创建并启动一个新的识别会话。
    async fn new() -> Result<Self> {
        let (event_tx, event_rx) = async_channel::unbounded();

        let recognizer = SpeechRecognizer::new().map_err(|e| Error::WindowsOperation {
            operation: "SpeechRecognizer::new",
            source: e,
        })?;

        // 编译约束（语言设置等）
        let compilation_result = recognizer
            .CompileConstraintsAsync()
            .map_err(|e| Error::WindowsOperation {
                operation: "CompileConstraintsAsync",
                source: e,
            })?
            .await
            .map_err(|e| Error::WindowsOperation {
                operation: "CompileConstraintsAsync.await",
                source: e,
            })?;

        let status = compilation_result.Status().map_err(|e| Error::WindowsOperation {
            operation: "CompilationResult.Status",
            source: e,
        })?;

        if status != SpeechRecognitionResultStatus::Success {
            return Err(Error::Other(anyhow::anyhow!(
                "Speech recognition constraint compilation failed: {:?}",
                status
            )));
        }

        // 获取连续识别会话
        let continuous_session = recognizer
            .ContinuousRecognitionSession()
            .map_err(|e| Error::WindowsOperation {
                operation: "ContinuousRecognitionSession",
                source: e,
            })?;

        // 注册 HypothesisGenerated 事件（在 SpeechRecognizer 上）
        let tx = event_tx.clone();
        let hypothesis_token = recognizer
            .HypothesisGenerated(&TypedEventHandler::<SpeechRecognizer, SpeechRecognitionHypothesisGeneratedEventArgs>::new(
                move |_sender, args| {
                    if let Some(args) = args.as_ref() {
                        if let Ok(hypothesis) = args.Hypothesis() {
                            if let Ok(text) = hypothesis.Text() {
                                let _ = tx.try_send(RealtimeSpeechEvent::Hypothesis {
                                    text: text.to_string(),
                                });
                            }
                        }
                    }
                    Ok(())
                },
            ))
            .map_err(|e| Error::WindowsOperation {
                operation: "HypothesisGenerated handler",
                source: e,
            })?;

        // 注册 ResultGenerated 事件
        let tx = event_tx.clone();
        let result_token = continuous_session
            .ResultGenerated(&TypedEventHandler::<SpeechContinuousRecognitionSession, SpeechContinuousRecognitionResultGeneratedEventArgs>::new(
                move |_sender, args| {
                    if let Some(args) = args.as_ref() {
                        if let Ok(result) = args.Result() {
                            if let Ok(status) = result.Status() {
                                if status == SpeechRecognitionResultStatus::Success {
                                    if let Ok(text) = result.Text() {
                                        let _ = tx.try_send(RealtimeSpeechEvent::Final {
                                            text: text.to_string(),
                                        });
                                    }
                                }
                            }
                        }
                    }
                    Ok(())
                },
            ))
            .map_err(|e| Error::WindowsOperation {
                operation: "ResultGenerated handler",
                source: e,
            })?;

        // 注册 Completed 事件
        let tx = event_tx.clone();
        let completed_token = continuous_session
            .Completed(&TypedEventHandler::<SpeechContinuousRecognitionSession, SpeechContinuousRecognitionCompletedEventArgs>::new(
                move |_sender, args| {
                    if let Some(args) = args.as_ref() {
                        if let Ok(status) = args.Status() {
                            match status {
                                SpeechRecognitionResultStatus::Success => {
                                    let _ = tx.try_send(RealtimeSpeechEvent::Completed);
                                }
                                SpeechRecognitionResultStatus::UserCanceled => {
                                    let _ = tx.try_send(RealtimeSpeechEvent::Canceled);
                                }
                                SpeechRecognitionResultStatus::TopicLanguageNotSupported
                                | SpeechRecognitionResultStatus::GrammarLanguageMismatch
                                | SpeechRecognitionResultStatus::GrammarCompilationFailure
                                | SpeechRecognitionResultStatus::AudioQualityFailure
                                | SpeechRecognitionResultStatus::Unknown
                                | SpeechRecognitionResultStatus::TimeoutExceeded
                                | SpeechRecognitionResultStatus::PauseLimitExceeded
                                | SpeechRecognitionResultStatus::NetworkFailure
                                | SpeechRecognitionResultStatus::MicrophoneUnavailable => {
                                    let _ = tx.try_send(RealtimeSpeechEvent::Error(format!(
                                        "Recognition completed with status: {status:?}"
                                    )));
                                }
                                _ => {
                                    let _ = tx.try_send(RealtimeSpeechEvent::Error(format!(
                                        "Recognition completed with unknown status: {status:?}"
                                    )));
                                }
                            }
                        }
                    } else {
                        let _ = tx.try_send(RealtimeSpeechEvent::Completed);
                    }
                    Ok(())
                },
            ))
            .map_err(|e| Error::WindowsOperation {
                operation: "Completed handler",
                source: e,
            })?;

        // 启动识别
        continuous_session
            .StartAsync()
            .map_err(|e| Error::from_windows_operation("StartAsync", e))?
            .await
            .map_err(|e| Error::from_windows_operation("StartAsync.await", e))?;

        Ok(Self {
            recognizer,
            event_rx,
            hypothesis_token,
            result_token,
            completed_token,
        })
    }

    /// 获取事件接收器。
    pub fn events(&self) -> Receiver<RealtimeSpeechEvent> {
        self.event_rx.clone()
    }

    /// 停止识别会话。
    pub async fn stop(&self) -> Result<()> {
        let continuous_session = self
            .recognizer
            .ContinuousRecognitionSession()
            .map_err(|e| Error::WindowsOperation {
                operation: "ContinuousRecognitionSession",
                source: e,
            })?;

        continuous_session
            .StopAsync()
            .map_err(|e| Error::WindowsOperation {
                operation: "StopAsync",
                source: e,
            })?
            .await
            .map_err(|e| Error::WindowsOperation {
                operation: "StopAsync.await",
                source: e,
            })?;

        Ok(())
    }

    /// 取消识别会话。
    pub async fn cancel(&self) -> Result<()> {
        let continuous_session = self
            .recognizer
            .ContinuousRecognitionSession()
            .map_err(|e| Error::WindowsOperation {
                operation: "ContinuousRecognitionSession",
                source: e,
            })?;

        continuous_session
            .CancelAsync()
            .map_err(|e| Error::WindowsOperation {
                operation: "CancelAsync",
                source: e,
            })?
            .await
            .map_err(|e| Error::WindowsOperation {
                operation: "CancelAsync.await",
                source: e,
            })?;

        Ok(())
    }
}

impl Drop for RealtimeSpeechSession {
    fn drop(&mut self) {
        // 移除事件处理器
        if let Ok(continuous_session) = self.recognizer.ContinuousRecognitionSession() {
            let _ = continuous_session.RemoveResultGenerated(self.result_token);
            let _ = continuous_session.RemoveCompleted(self.completed_token);
        }
        let _ = self.recognizer.RemoveHypothesisGenerated(self.hypothesis_token);

        // 关闭识别器
        let _ = self.recognizer.Close();
    }
}
