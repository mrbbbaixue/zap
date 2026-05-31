use super::{
    EditOrigin, EditorAction, EditorView, InteractionState, PlainTextEditorViewAction,
    SelectionInsertion, UpdateBufferOption, VoiceTranscriptionOptions,
};
use crate::ai::blocklist::InputType;
use crate::appearance::Appearance;
use crate::editor::EditorElement;
use crate::server::telemetry::TelemetryEvent;
use crate::settings::{AISettings, VoiceInputToggleKey};
use crate::themes::theme::Fill;
use crate::ui_components::buttons::{icon_button, icon_button_with_color};
use crate::ui_components::icons;
use crate::view_components::{FeaturePopup, NewFeaturePopupLabel};
use crate::workspace::ToastStack;
use crate::workspaces::user_workspaces::UserWorkspaces;
use settings::Setting as _;
use std::time::{Duration, Instant};
use voice_input::{StartListeningError, VoiceSessionError, VoiceSessionEvent};
use warp_core::send_telemetry_from_ctx;
use warp_core::ui::theme::color::internal_colors;
use warp_core::ui::theme::AnsiColorIdentifier;
use warpui::elements;
use warpui::elements::{Container, CornerRadius, Icon, Radius};
use warpui::platform::Cursor;
use warpui::text_layout::TextStyle;
use warpui::ui_components::button::ButtonTooltipPosition;
use warpui::ui_components::components::{Coords, UiComponent, UiComponentStyles};
use warpui::ViewHandle;
use warpui::{AppContext, Element, SingletonEntity, ViewContext};

const MICROPHONE_ACCESS_ERROR_ID: &str = "MICROPHONE_ACCESS_ERROR";
const SPEECH_PRIVACY_ERROR_ID: &str = "SPEECH_PRIVACY_ERROR";
const NUM_TIMES_TO_SHOW_VOICE_NEW_FEATURE_POPUP: usize = 4;

/// Windows Alt 键会触发瞬时 press+release（菜单循环导致），
/// 按住短暂时间内的 release 事件需要被忽略。
const MIN_KEY_HOLD_DURATION: Duration = Duration::from_millis(150);

#[derive(Debug, Default, Clone)]
pub(super) enum VoiceInputState {
    #[default]
    Stopped,

    /// 正在监听语音输入。
    Listening,
}

impl VoiceInputState {
    pub(super) fn is_active(&self) -> bool {
        matches!(self, VoiceInputState::Listening)
    }

    pub(super) fn icon(&self) -> Option<icons::Icon> {
        match self {
            VoiceInputState::Listening => Some(icons::Icon::Microphone),
            VoiceInputState::Stopped => None,
        }
    }
}

impl EditorView {
    pub(super) fn is_voice_input_active(&self) -> bool {
        self.voice_input_state.is_active()
    }

    pub(super) fn create_voice_new_feature_popup(
        ctx: &mut ViewContext<EditorView>,
    ) -> ViewHandle<FeaturePopup> {
        let voice_new_feature_popup = ctx.add_typed_action_view(|_| {
            FeaturePopup::new_feature(NewFeaturePopupLabel::FromString(crate::t!(
                "voice-try-input"
            )))
        });

        ctx.subscribe_to_view(&voice_new_feature_popup, |_me, _, event, ctx| {
            if matches!(
                event,
                crate::view_components::NewFeaturePopupEvent::Dismissed
            ) {
                AISettings::handle(ctx).update(ctx, |settings, ctx| {
                    warp_core::report_if_error!(settings
                        .dismissed_voice_input_new_feature_popup
                        .set_value(true, ctx));
                });
                ctx.notify();
            }
        });

        voice_new_feature_popup
    }

    pub(super) fn should_show_voice_new_feature_popup(&self, app: &AppContext) -> bool {
        let ai_settings = AISettings::handle(app).as_ref(app);
        let voice_input = voice_input::VoiceInput::handle(app).as_ref(app);

        let num_times_entered_agent_mode = *ai_settings.entered_agent_mode_num_times;
        let manually_dismissed_voice_input_new_feature_popup =
            *ai_settings.dismissed_voice_input_new_feature_popup;
        let explicitly_interacted_with_voice = *ai_settings.explicitly_interacted_with_voice;

        num_times_entered_agent_mode <= NUM_TIMES_TO_SHOW_VOICE_NEW_FEATURE_POPUP
            && !manually_dismissed_voice_input_new_feature_popup
            && !explicitly_interacted_with_voice
            && !voice_input.should_suppress_new_feature_popup
    }

    /// Configures an [`EditorElement`] for the current voice input state.
    pub(super) fn configure_editor_element_voice(
        &self,
        editor_element: EditorElement,
        appearance: &Appearance,
    ) -> EditorElement {
        if let Some(icon) = self.voice_input_state.icon() {
            editor_element.with_voice_input_cursor_icon(
                Container::new(
                    Icon::new(icon.into(), internal_colors::neutral_1(appearance.theme())).finish(),
                )
                .with_background(Fill::Solid(appearance.theme().accent().into()))
                .with_uniform_padding(4.)
                .with_corner_radius(CornerRadius::with_all(Radius::Pixels(8.)))
                .finish(),
            )
        } else {
            editor_element
        }
    }

    pub fn update_voice_transcription_options(
        &mut self,
        options: VoiceTranscriptionOptions,
        ctx: &mut ViewContext<Self>,
    ) {
        if !UserWorkspaces::handle(ctx).as_ref(ctx).is_voice_enabled() {
            return;
        }

        log::debug!("update_voice_transcription_options: {options:?}");
        self.voice_transcription_options = options;
        if !self.voice_transcription_options.is_enabled() {
            self.stop_voice_input(true, ctx);
        }
        ctx.notify();
    }

    pub(super) fn voice_options(ctx: &mut ViewContext<Self>) -> VoiceTranscriptionOptions {
        let ai_settings_handle = AISettings::handle(ctx);
        if ai_settings_handle.as_ref(ctx).is_voice_input_enabled(ctx) {
            VoiceTranscriptionOptions::Enabled { show_button: false }
        } else {
            VoiceTranscriptionOptions::Disabled
        }
    }

    pub(super) fn stop_voice_input(&mut self, cancel: bool, ctx: &mut ViewContext<Self>) {
        if !UserWorkspaces::handle(ctx).as_ref(ctx).is_voice_enabled() {
            return;
        }

        let voice_input = voice_input::VoiceInput::handle(ctx);
        if voice_input.as_ref(ctx).is_listening() {
            log::debug!("Stopping voice input, cancel: {cancel}");
            voice_input.update(ctx, |voice_input, _ctx| {
                if cancel {
                    voice_input.abort_listening();
                } else if let Err(e) = voice_input.stop_listening() {
                    log::error!("Failed to stop voice input: {e:?}");
                }
            });
        }
        if cancel {
            self.clear_voice_hypothesis(ctx);
        }
        self.set_voice_input_state(VoiceInputState::Stopped, ctx);
        ctx.notify();
    }

    fn voice_error_toast(&mut self, message: &str, ctx: &mut ViewContext<Self>) {
        let window_id = ctx.window_id();
        ToastStack::handle(ctx).update(ctx, |toast_stack, ctx| {
            let toast = crate::view_components::DismissibleToast::error(message.to_string());
            toast_stack.add_ephemeral_toast(toast, window_id, ctx);
        });
    }

    pub fn toggle_voice_input(
        &mut self,
        source: &voice_input::VoiceInputToggledFrom,
        ctx: &mut ViewContext<Self>,
    ) -> bool {
        if !UserWorkspaces::handle(ctx).as_ref(ctx).is_voice_enabled() {
            return false;
        }

        if !matches!(
            Self::voice_options(ctx),
            VoiceTranscriptionOptions::Enabled { .. }
        ) {
            return false;
        }

        log::debug!(
            "Toggling voice input from {:?} for current state: {:?}",
            source,
            self.voice_input_state
        );

        match *source {
            voice_input::VoiceInputToggledFrom::Button => {
                ctx.focus_self();
            }
            voice_input::VoiceInputToggledFrom::Key { state } => {
                if !self.focused {
                    return false;
                }

                match &self.voice_input_state {
                    VoiceInputState::Stopped => {
                        if matches!(state, warpui::event::KeyState::Released) {
                            return false;
                        }
                        // 记录按键按下时间，用于后续释放时的去抖判断。
                        self.voice_input_key_press_time = Some(Instant::now());
                    }
                    VoiceInputState::Listening => {
                        if matches!(state, warpui::event::KeyState::Pressed) {
                            return false;
                        }
                        if !Self::voice_input_started_from_key_press(ctx) {
                            return false;
                        }
                        // Windows 上 Alt 键会在按下后立即由系统菜单循环触发虚假
                        // release，需忽略按住时间过短的释放事件。
                        if let Some(press_time) = self.voice_input_key_press_time {
                            if press_time.elapsed() < MIN_KEY_HOLD_DURATION {
                                log::debug!(
                                    "Voice input key released too quickly ({:?} < {:?}), ignoring",
                                    press_time.elapsed(),
                                    MIN_KEY_HOLD_DURATION,
                                );
                                return false;
                            }
                        }
                        self.voice_input_key_press_time = None;
                    }
                }
            }
        }

        match &self.voice_input_state {
            VoiceInputState::Stopped => {
                if !self.voice_transcription_options.is_enabled() {
                    return false;
                }

                if !crate::ai::AIRequestUsageModel::handle(ctx)
                    .as_ref(ctx)
                    .can_request_voice()
                {
                    self.voice_error_toast(&crate::t!("editor-voice-limit-hit-toast"), ctx);
                    return false;
                }

                if self.focused || matches!(*source, voice_input::VoiceInputToggledFrom::Button) {
                    // 启动语音识别会话
                    let session_result = voice_input::VoiceInput::handle(ctx)
                        .update(ctx, |voice_input, ctx| {
                            voice_input.start_listening(source.clone(), ctx)
                        });

                    let event_rx = match session_result {
                        Ok(rx) => rx,
                        Err(e) => {
                            match e {
                                StartListeningError::AccessDenied => {
                                    Self::show_microphone_access_toast(ctx);
                                }
                                _ => {
                                    log::error!("Failed to start voice input: {e:?}");
                                }
                            }
                            ctx.notify();
                            return false;
                        }
                    };

                    // 立即转换到 Listening 状态
                    self.set_voice_input_state(VoiceInputState::Listening, ctx);

                    // 发送遥测
                    let is_udi_enabled = crate::settings::InputSettings::handle(ctx)
                        .as_ref(ctx)
                        .is_universal_developer_input_enabled(ctx);
                    let current_input_mode = if self.is_ai_input {
                        InputType::AI
                    } else {
                        InputType::Shell
                    };
                    send_telemetry_from_ctx!(
                        TelemetryEvent::VoiceInputUsed {
                            action: "start".to_string(),
                            session_duration_ms: None,
                            is_udi_enabled,
                            current_input_mode,
                        },
                        ctx
                    );

                    ctx.spawn_stream_local(event_rx, Self::handle_voice_event, |_, _| {});

                    if matches!(*source, voice_input::VoiceInputToggledFrom::Button) {
                        let window_id = ctx.window_id();
                        AISettings::handle(ctx).update(ctx, |settings, ctx| {
                            if let Some(toggle_key) = settings.maybe_setup_first_time_voice(ctx) {
                                ToastStack::handle(ctx).update(ctx, |toast_stack, ctx| {
                                    let toast = crate::view_components::DismissibleToast::success(
                                        crate::t!(
                                            "voice-input-enabled-toast",
                                            key = toggle_key.display_name()
                                        ),
                                    );
                                    toast_stack.add_ephemeral_toast(toast, window_id, ctx);
                                });
                            }
                        });
                    }
                    ctx.notify();
                    return true;
                }
            }
            VoiceInputState::Listening => {
                self.stop_voice_input(false, ctx);
            }
        }
        ctx.notify();
        false
    }

    fn voice_input_started_from_key_press(ctx: &AppContext) -> bool {
        matches!(
            voice_input::VoiceInput::handle(ctx).as_ref(ctx).state(),
            voice_input::VoiceInputState::Listening {
                enabled_from: voice_input::VoiceInputToggledFrom::Key {
                    state: warpui::event::KeyState::Pressed
                },
                ..
            }
        )
    }

    fn show_microphone_access_toast(ctx: &mut ViewContext<Self>) {
        let active_window_id = ctx.window_id();
        ToastStack::handle(ctx).update(ctx, move |toast_stack, ctx| {
            let mut toast = crate::view_components::DismissibleToast::error(String::from(
                crate::t!("voice-input-microphone-access-error"),
            ));
            toast = toast.with_object_id(MICROPHONE_ACCESS_ERROR_ID.to_string());
            toast_stack.add_ephemeral_toast(toast, active_window_id, ctx);
        });
    }

    fn show_speech_privacy_toast(ctx: &mut ViewContext<Self>) {
        let active_window_id = ctx.window_id();
        ToastStack::handle(ctx).update(ctx, move |toast_stack, ctx| {
            let mut toast = crate::view_components::DismissibleToast::error(String::from(
                crate::t!("voice-input-speech-privacy-error"),
            ));
            toast = toast.with_object_id(SPEECH_PRIVACY_ERROR_ID.to_string());
            toast_stack.add_ephemeral_toast(toast, active_window_id, ctx);
        });
    }

    fn set_voice_input_state(
        &mut self,
        voice_input_state: VoiceInputState,
        ctx: &mut ViewContext<Self>,
    ) {
        let was_active = self.is_voice_input_active();
        let is_listening = matches!(voice_input_state, VoiceInputState::Listening);
        let will_be_active = matches!(voice_input_state, VoiceInputState::Listening);

        if !was_active && will_be_active {
            self.interaction_state_before_voice = Some(self.interaction_state(ctx));
            self.set_interaction_state(super::InteractionState::Selectable, ctx);
            self.voice_input_state = voice_input_state;
        } else if was_active && !will_be_active {
            self.voice_input_state = voice_input_state;
            if let Some(state) = self.interaction_state_before_voice.take() {
                self.set_interaction_state(state, ctx);
            }
        } else {
            self.voice_input_state = voice_input_state;
        }

        ctx.emit(super::Event::VoiceStateUpdated {
            is_listening,
            is_transcribing: false,
        });
    }

    fn with_voice_editor_edit(
        &mut self,
        ctx: &mut ViewContext<Self>,
        edit: impl FnOnce(&mut Self, &mut ViewContext<Self>),
    ) {
        let previous_state = self.interaction_state(ctx);
        self.editor_model.update(ctx, |model, _| {
            model.set_interaction_state(InteractionState::Editable);
        });
        edit(self, ctx);
        self.editor_model.update(ctx, |model, _| {
            model.set_interaction_state(previous_state);
        });
    }

    fn update_voice_hypothesis(&mut self, text: &str, ctx: &mut ViewContext<Self>) {
        if text.is_empty() {
            return;
        }

        let cursor_colors = (self.get_cursor_colors_fn)(ctx);
        let marked_text_style =
            Some(TextStyle::new().with_underline_color(cursor_colors.cursor.into()));
        let selected_range_end = text.chars().count();
        self.with_voice_editor_edit(ctx, |me, ctx| {
            me.edit(
                ctx,
                super::model::Edits::new().with_update_buffer_options(
                    PlainTextEditorViewAction::UpdateMarkedText,
                    EditOrigin::UserTyped,
                    UpdateBufferOption::IsEphemeral,
                    |editor_model, ctx| {
                        editor_model.update_marked_text(
                            text,
                            marked_text_style,
                            &(selected_range_end..selected_range_end),
                            ctx,
                        );
                    },
                ),
            );
        });
        self.voice_hypothesis_active = true;
    }

    fn commit_voice_text(&mut self, text: &str, ctx: &mut ViewContext<Self>) {
        if text.is_empty() && !self.voice_hypothesis_active {
            return;
        }

        let had_hypothesis = self.voice_hypothesis_active;
        let action = PlainTextEditorViewAction::from_inserted_str(text);
        self.with_voice_editor_edit(ctx, |me, ctx| {
            me.edit(
                ctx,
                super::model::Edits::new().with_update_buffer(
                    action,
                    EditOrigin::UserTyped,
                    |editor_model, ctx| {
                        if had_hypothesis {
                            editor_model.clear_marked_text_and_commit(text, ctx);
                        } else {
                            editor_model.insert_internal(text, None, SelectionInsertion::No, ctx);
                        }
                    },
                ),
            );
        });
        self.voice_hypothesis_active = false;
    }

    fn commit_voice_hypothesis(&mut self, ctx: &mut ViewContext<Self>) {
        if !self.voice_hypothesis_active {
            return;
        }

        self.with_voice_editor_edit(ctx, |me, ctx| {
            me.edit(
                ctx,
                super::model::Edits::new().with_update_buffer(
                    PlainTextEditorViewAction::UpdateMarkedText,
                    EditOrigin::UserTyped,
                    |editor_model, ctx| {
                        editor_model.commit_incomplete_marked_text(ctx);
                    },
                ),
            );
        });
        self.voice_hypothesis_active = false;
    }

    fn clear_voice_hypothesis(&mut self, ctx: &mut ViewContext<Self>) {
        if !self.voice_hypothesis_active {
            return;
        }

        self.with_voice_editor_edit(ctx, |me, ctx| {
            me.editor_model
                .update(ctx, |editor_model, ctx| editor_model.clear_marked_text(ctx));
        });
        self.voice_hypothesis_active = false;
    }

    /// 处理语音识别事件。
    pub(super) fn handle_voice_event(
        &mut self,
        event: VoiceSessionEvent,
        ctx: &mut ViewContext<Self>,
    ) {
        if !UserWorkspaces::handle(ctx).as_ref(ctx).is_voice_enabled() {
            return;
        }

        let is_udi_enabled = crate::settings::InputSettings::handle(ctx)
            .as_ref(ctx)
            .is_universal_developer_input_enabled(ctx);
        let current_input_mode = if self.is_ai_input {
            InputType::AI
        } else {
            InputType::Shell
        };

        match event {
            VoiceSessionEvent::Hypothesis { text } => {
                // 中间结果会替换同一段临时文本，避免把每次假设都追加进输入框。
                log::debug!("Voice hypothesis: {text}");
                self.update_voice_hypothesis(&text, ctx);
            }
            VoiceSessionEvent::Final { text } => {
                // 最终结果：提交文本到 editor buffer
                log::debug!("Voice final: {text}");
                self.commit_voice_text(&text, ctx);
            }
            VoiceSessionEvent::Completed {
                session_duration_ms,
            } => {
                log::info!("Voice session completed");
                self.commit_voice_hypothesis(ctx);
                self.set_voice_input_state(VoiceInputState::Stopped, ctx);

                send_telemetry_from_ctx!(
                    TelemetryEvent::VoiceInputUsed {
                        action: "stop".to_string(),
                        session_duration_ms: Some(session_duration_ms),
                        is_udi_enabled,
                        current_input_mode,
                    },
                    ctx
                );
            }
            VoiceSessionEvent::Canceled {
                session_duration_ms,
            } => {
                log::info!("Voice session canceled");
                self.clear_voice_hypothesis(ctx);
                self.set_voice_input_state(VoiceInputState::Stopped, ctx);

                send_telemetry_from_ctx!(
                    TelemetryEvent::VoiceInputUsed {
                        action: "cancel".to_string(),
                        session_duration_ms,
                        is_udi_enabled,
                        current_input_mode,
                    },
                    ctx
                );
            }
            VoiceSessionEvent::Error(err) => {
                log::error!("Voice session error: {err}");
                match err {
                    VoiceSessionError::SpeechPrivacyPolicyNotAccepted => {
                        Self::show_speech_privacy_toast(ctx);
                    }
                    VoiceSessionError::Other(_) => {
                        self.voice_error_toast(&crate::t!("editor-voice-error-toast"), ctx);
                    }
                }
                self.clear_voice_hypothesis(ctx);
                self.set_voice_input_state(VoiceInputState::Stopped, ctx);
            }
        }
        ctx.notify();
    }

    fn render_voice_transcription_button_tooltip(
        &self,
        appearance: &crate::appearance::Appearance,
        app: &AppContext,
    ) -> Box<dyn FnOnce() -> Box<dyn Element>> {
        let tooltip_background = appearance.theme().surface_1().into_solid();
        let tooltip_text_color = appearance
            .theme()
            .main_text_color(tooltip_background.into())
            .into_solid();
        let ui_builder = appearance.ui_builder().clone();

        let microphone_access_state = app.microphone_access_state();
        let mic_access_denied = matches!(
            microphone_access_state,
            warpui::platform::MicrophoneAccessState::Restricted
                | warpui::platform::MicrophoneAccessState::Denied
        );

        let modifier_key = AISettings::handle(app).as_ref(app).voice_input_toggle_key;
        let tooltip_text = if mic_access_denied {
            crate::t!("voice-transcription-disabled-microphone")
        } else if modifier_key == VoiceInputToggleKey::None {
            crate::t!("voice-transcription")
        } else {
            crate::t!(
                "voice-transcription-hold-key",
                key = modifier_key.display_name().to_lowercase()
            )
        };

        Box::new(move || {
            let tool_tip_style = UiComponentStyles {
                background: Some(elements::Fill::Solid(tooltip_background)),
                font_color: Some(tooltip_text_color),
                ..Default::default()
            };

            ui_builder
                .tool_tip(tooltip_text)
                .with_style(tool_tip_style)
                .build()
                .finish()
        })
    }

    pub(super) fn render_voice_transcription_button(
        &self,
        icon_size: f32,
        appearance: &crate::appearance::Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let mut button = if voice_input::VoiceInput::handle(app)
            .as_ref(app)
            .is_listening()
        {
            icon_button_with_color(
                appearance,
                icons::Icon::Stop,
                true,
                self.voice_transcription_button_mouse_handle.clone(),
                Fill::Solid(
                    AnsiColorIdentifier::Red
                        .to_ansi_color(&appearance.theme().terminal_colors().normal)
                        .into(),
                ),
            )
        } else {
            icon_button(
                appearance,
                icons::Icon::Microphone,
                false,
                self.voice_transcription_button_mouse_handle.clone(),
            )
        };

        button = button.with_style(UiComponentStyles {
            width: Some(icon_size),
            height: Some(icon_size),
            padding: Some(Coords::uniform(icon_size / 10.)),
            ..Default::default()
        });

        if !self.should_show_voice_new_feature_popup(app) {
            button = button
                .with_tooltip_position(ButtonTooltipPosition::Above)
                .with_tooltip(self.render_voice_transcription_button_tooltip(appearance, app));
        }

        warpui::elements::SavePosition::new(
            button
                .build()
                .on_click(move |ctx, _, _| {
                    ctx.dispatch_typed_action(EditorAction::ToggleVoiceInput(
                        voice_input::VoiceInputToggledFrom::Button,
                    ));
                })
                .with_cursor(Cursor::PointingHand)
                .finish(),
            "voice_transcription_button",
        )
        .finish()
    }
}
