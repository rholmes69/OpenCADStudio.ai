//! The AI Settings dialog (AISETTINGS): edits the shared provider settings
//! in `crate::ai::settings` and saves them when OK is pressed.

use super::{Message, ModalKind, OpenCADStudio};
use crate::ai::settings::FeatureRoute;
use crate::ai::{ChatMessage, ChatRequest, ProviderKind};
use crate::ui::window::ai_settings::{AiSettingsMsg, ProviderChoice, State};
use iced::Task;

impl OpenCADStudio {
    pub(in crate::app) fn open_ai_settings(&mut self) {
        let (settings, keys) = crate::ai::settings::current();
        self.ai_settings_dialog = Some(State::new(settings, keys));
        self.active_modal = Some(ModalKind::AiSettings);
        self.reset_modal_geometry();
    }

    pub(in crate::app) fn on_ai_settings(&mut self, msg: AiSettingsMsg) -> Task<Message> {
        let Some(state) = self.ai_settings_dialog.as_mut() else {
            return Task::none();
        };
        state.error = None;
        match msg {
            AiSettingsMsg::Tab(tab) => state.tab = tab,
            AiSettingsMsg::DefaultProvider(kind) => state.settings.default_provider = kind,
            AiSettingsMsg::Url(kind, v) => state.settings.provider_mut(kind).base_url = v,
            AiSettingsMsg::Model(kind, v) => state.settings.provider_mut(kind).default_model = v,
            AiSettingsMsg::Key(kind, v) => {
                if let Some(slot) = state.keys.stored_mut(kind) {
                    *slot = v;
                }
            }
            AiSettingsMsg::ToggleKeyVisible => state.show_keys = !state.show_keys,
            AiSettingsMsg::RefreshModels(kind) => {
                state.loading.insert(kind);
                state.model_status.remove(&kind);
                let base = state.settings.provider(kind).base_url.clone();
                let key = state.keys.get(kind);
                return Task::perform(
                    crate::ai::run_blocking(move || {
                        crate::ai::client::list_models(kind, &base, key.as_deref())
                    }),
                    move |r| Message::AiSettings(AiSettingsMsg::ModelsLoaded(kind, r)),
                );
            }
            AiSettingsMsg::ModelsLoaded(kind, result) => {
                state.loading.remove(&kind);
                match result {
                    Ok(models) => {
                        let vision = models.iter().filter(|m| m.vision).count();
                        state.model_status.insert(
                            kind,
                            Ok(format!("{} models ({vision} with vision)", models.len())),
                        );
                        crate::ai::set_cached_models(kind, models.clone());
                        state.models.insert(kind, models);
                    }
                    Err(e) => {
                        state.model_status.insert(kind, Err(e));
                    }
                }
            }
            AiSettingsMsg::FeatureProvider(feature, choice) => {
                let entry = state.settings.features.entry(feature).or_default();
                entry.provider = match choice {
                    ProviderChoice::Default => None,
                    ProviderChoice::Fixed(kind) => Some(kind),
                };
                prune(&mut state.settings.features, feature);
            }
            AiSettingsMsg::FeatureModel(feature, model) => {
                state.settings.features.entry(feature).or_default().model = model;
                prune(&mut state.settings.features, feature);
            }
            AiSettingsMsg::LocalOnly(v) => state.settings.local_only = v,
            AiSettingsMsg::ConfirmBeforeSend(v) => state.settings.confirm_before_send = v,
            AiSettingsMsg::Timeout(v) => state.timeout_text = v,
            AiSettingsMsg::MaxTokens(v) => state.tokens_text = v,
            AiSettingsMsg::Test => {
                let kind = state.settings.default_provider;
                let model = state
                    .settings
                    .provider(kind)
                    .default_model
                    .trim()
                    .to_string();
                match state.settings.resolve(kind, model, &state.keys) {
                    Ok(mut route) => {
                        route.max_output_tokens = 16;
                        route.timeout_secs = route.timeout_secs.min(60);
                        state.testing = true;
                        state.test_result = None;
                        let request = ChatRequest {
                            messages: vec![ChatMessage::user("Reply with the single word: OK")],
                            temperature: Some(0.0),
                            json_mode: false,
                        };
                        return Task::perform(
                            crate::ai::run_blocking(move || {
                                let started = std::time::Instant::now();
                                let reply = crate::ai::client::chat(&route, &request)?;
                                Ok(format!(
                                    "{} answered \"{}\" in {:.1} s",
                                    if reply.model.is_empty() {
                                        route.model
                                    } else {
                                        reply.model
                                    },
                                    reply.content.trim().chars().take(40).collect::<String>(),
                                    started.elapsed().as_secs_f32()
                                ))
                            }),
                            |r| Message::AiSettings(AiSettingsMsg::TestDone(r)),
                        );
                    }
                    Err(e) => state.test_result = Some(Err(e)),
                }
            }
            AiSettingsMsg::TestDone(result) => {
                state.testing = false;
                state.test_result = Some(result);
            }
            AiSettingsMsg::Ok => return self.apply_ai_settings(),
        }
        Task::none()
    }

    fn apply_ai_settings(&mut self) -> Task<Message> {
        let Some(state) = self.ai_settings_dialog.as_mut() else {
            return Task::none();
        };
        let timeout = match state.timeout_text.trim().parse::<u64>() {
            Ok(v) if (5..=3600).contains(&v) => v,
            _ => {
                state.error =
                    Some("Timeout must be a whole number of seconds from 5 to 3600.".into());
                return Task::none();
            }
        };
        let tokens = match state.tokens_text.trim().parse::<u32>() {
            Ok(v) if (64..=200_000).contains(&v) => v,
            _ => {
                state.error = Some("Max reply tokens must be from 64 to 200000.".into());
                return Task::none();
            }
        };
        for kind in ProviderKind::ALL {
            let url = state.settings.provider(kind).base_url.trim().to_string();
            if !url.is_empty() && !url.starts_with("http://") && !url.starts_with("https://") {
                state.error = Some(format!(
                    "The {kind} URL must start with http:// or https://."
                ));
                return Task::none();
            }
            state.settings.provider_mut(kind).base_url = url;
        }
        let mut settings = state.settings.clone();
        settings.timeout_secs = timeout;
        settings.max_output_tokens = tokens;
        let keys = state.keys.clone();
        self.ai_settings_dialog = None;
        self.active_modal = None;
        match crate::ai::settings::save(settings, keys) {
            Ok(()) => self.command_line.push_info("AI settings saved."),
            Err(e) => self
                .command_line
                .push_error(&format!("AI settings could not be saved: {e}")),
        }
        Task::none()
    }
}

/// Drop a feature override that no longer overrides anything.
fn prune(
    features: &mut std::collections::BTreeMap<crate::ai::AiFeature, FeatureRoute>,
    feature: crate::ai::AiFeature,
) {
    if features
        .get(&feature)
        .is_some_and(|r| r.provider.is_none() && r.model.trim().is_empty())
    {
        features.remove(&feature);
    }
}
