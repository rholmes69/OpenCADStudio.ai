//! AI Settings dialog (AISETTINGS): providers and keys, per-feature model
//! routing, and the privacy / cost switches every AI feature shares.

use crate::ai::settings::{ApiKeys, FeatureRoute};
use crate::ai::{AiFeature, AiSettings, ModelInfo, ProviderKind};
use crate::app::Message;
use crate::ui::style::form::{
    dialog_button, field_style, hdivider, labeled_field, labeled_pick_list, section_label,
};
use iced::widget::{
    button, checkbox, column, container, pick_list, row, scrollable, text, text_input, Space,
};
use iced::{Background, Border, Element, Fill, Theme};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Providers,
    Features,
    Privacy,
}

/// A feature's provider choice: follow the default, or a fixed provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderChoice {
    Default,
    Fixed(ProviderKind),
}

impl ProviderChoice {
    pub const ALL: [ProviderChoice; 4] = [
        ProviderChoice::Default,
        ProviderChoice::Fixed(ProviderKind::OpenRouter),
        ProviderChoice::Fixed(ProviderKind::Ollama),
        ProviderChoice::Fixed(ProviderKind::OpenAiCompatible),
    ];
}

impl std::fmt::Display for ProviderChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProviderChoice::Default => f.write_str("Default provider"),
            ProviderChoice::Fixed(kind) => f.write_str(kind.label()),
        }
    }
}

#[derive(Debug, Clone)]
pub enum AiSettingsMsg {
    Tab(Tab),
    DefaultProvider(ProviderKind),
    Url(ProviderKind, String),
    Model(ProviderKind, String),
    Key(ProviderKind, String),
    ToggleKeyVisible,
    RefreshModels(ProviderKind),
    ModelsLoaded(ProviderKind, Result<Vec<ModelInfo>, String>),
    FeatureProvider(AiFeature, ProviderChoice),
    FeatureModel(AiFeature, String),
    LocalOnly(bool),
    ConfirmBeforeSend(bool),
    Timeout(String),
    MaxTokens(String),
    Test,
    TestDone(Result<String, String>),
    Ok,
}

pub(crate) struct State {
    pub tab: Tab,
    pub settings: AiSettings,
    pub keys: ApiKeys,
    pub show_keys: bool,
    pub models: HashMap<ProviderKind, Vec<ModelInfo>>,
    pub model_status: HashMap<ProviderKind, Result<String, String>>,
    pub loading: HashSet<ProviderKind>,
    pub testing: bool,
    pub test_result: Option<Result<String, String>>,
    pub timeout_text: String,
    pub tokens_text: String,
    pub error: Option<String>,
}

impl State {
    pub fn new(settings: AiSettings, keys: ApiKeys) -> Self {
        let mut models = HashMap::new();
        for kind in ProviderKind::ALL {
            let cached = crate::ai::cached_models(kind);
            if !cached.is_empty() {
                models.insert(kind, cached);
            }
        }
        Self {
            tab: Tab::Providers,
            timeout_text: settings.timeout_secs.to_string(),
            tokens_text: settings.max_output_tokens.to_string(),
            settings,
            keys,
            show_keys: false,
            models,
            model_status: HashMap::new(),
            loading: HashSet::new(),
            testing: false,
            test_result: None,
            error: None,
        }
    }
}

fn msg(m: AiSettingsMsg) -> Message {
    Message::AiSettings(m)
}

fn tab_button(state: &State, tab: Tab, label: &'static str) -> Element<'static, Message> {
    let is_active = state.tab == tab;
    button(text(label).size(11))
        .on_press(msg(AiSettingsMsg::Tab(tab)))
        .style(move |theme: &Theme, status| {
            let palette = theme.palette();
            let pair = match (is_active, status) {
                (true, _) => palette.primary.strong,
                (false, button::Status::Hovered | button::Status::Pressed) => {
                    palette.background.strong
                }
                _ => palette.background.weak,
            };
            button::Style {
                background: Some(Background::Color(pair.color)),
                text_color: pair.text,
                border: Border {
                    color: palette.background.neutral.color,
                    width: 1.0,
                    radius: iced::border::Radius {
                        top_left: 4.0,
                        top_right: 4.0,
                        bottom_right: 0.0,
                        bottom_left: 0.0,
                    },
                },
                ..Default::default()
            }
        })
        .padding([4, 8])
        .into()
}

fn status_line(result: Option<&Result<String, String>>) -> Element<'_, Message> {
    match result {
        Some(Ok(s)) => text(s.as_str()).size(11).style(text::success).into(),
        Some(Err(e)) => text(e.as_str()).size(11).style(text::danger).into(),
        None => Space::new().height(0).into(),
    }
}

fn model_picker<'a>(
    state: &'a State,
    kind: ProviderKind,
    current: &'a str,
    on_text: impl Fn(String) -> Message + 'a,
    on_pick: impl Fn(String) -> Message + 'a,
    placeholder: &'a str,
) -> Element<'a, Message> {
    let ids: Vec<String> = state
        .models
        .get(&kind)
        .map(|m| {
            m.iter()
                .map(|info| {
                    if info.vision {
                        format!("{}  [vision]", info.id)
                    } else {
                        info.id.clone()
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    let field = text_input(placeholder, current)
        .on_input(on_text)
        .style(field_style)
        .size(12)
        .width(Fill);
    if ids.is_empty() {
        return field.into();
    }
    let picker = pick_list(None::<String>, ids, |s| s.clone())
        .placeholder("Choose…")
        .on_select(move |s: String| on_pick(s.trim_end_matches("  [vision]").to_string()))
        .text_size(12)
        .padding([3, 6])
        .width(150);
    row![field, picker].spacing(6).align_y(iced::Center).into()
}

fn provider_section<'a>(state: &'a State, kind: ProviderKind) -> Element<'a, Message> {
    let p = state.settings.provider(kind);
    let mut col = column![section_label(kind.label().into())].spacing(6);
    col = col.push(labeled_field(
        "URL".into(),
        &p.base_url,
        move |v| msg(AiSettingsMsg::Url(kind, v)),
        360.0,
    ));
    if kind != ProviderKind::Ollama {
        let env = ApiKeys::env_override(kind).is_some();
        let key_row: Element<'a, Message> = if env {
            text(format!(
                "Using the key from the {} environment variable.",
                kind.key_env_var().unwrap_or("")
            ))
            .size(11)
            .into()
        } else {
            let input = text_input(
                if kind == ProviderKind::OpenRouter {
                    "sk-or-…"
                } else {
                    "optional"
                },
                state.keys.stored(kind),
            )
            .on_input(move |v| msg(AiSettingsMsg::Key(kind, v)))
            .secure(!state.show_keys)
            .style(field_style)
            .size(12)
            .width(Fill);
            row![
                input,
                button(text(if state.show_keys { "Hide" } else { "Show" }).size(11))
                    .on_press(msg(AiSettingsMsg::ToggleKeyVisible))
                    .padding([3, 8]),
            ]
            .spacing(6)
            .align_y(iced::Center)
            .into()
        };
        col = col.push(
            row![text("API key").size(11).width(92), key_row]
                .spacing(8)
                .align_y(iced::Center),
        );
    }
    col = col.push(
        row![
            text("Default model").size(11).width(92),
            model_picker(
                state,
                kind,
                &p.default_model,
                move |v| msg(AiSettingsMsg::Model(kind, v)),
                move |v| msg(AiSettingsMsg::Model(kind, v)),
                "model id",
            ),
        ]
        .spacing(8)
        .align_y(iced::Center),
    );
    let loading = state.loading.contains(&kind);
    let refresh = button(
        text(if loading {
            "Loading…"
        } else {
            "Refresh models"
        })
        .size(11),
    )
    .on_press_maybe((!loading).then(|| msg(AiSettingsMsg::RefreshModels(kind))))
    .padding([3, 8]);
    col = col.push(
        row![
            Space::new().width(92),
            refresh,
            status_line(state.model_status.get(&kind))
        ]
        .spacing(8)
        .align_y(iced::Center),
    );
    container(col)
        .style(container::bordered_box)
        .padding(10)
        .width(Fill)
        .into()
}

pub(crate) fn view(state: &State, sizing: crate::ui::modal::ModalSizing) -> Element<'_, Message> {
    let tabs = row![
        tab_button(state, Tab::Providers, "Providers"),
        tab_button(state, Tab::Features, "Features"),
        tab_button(state, Tab::Privacy, "Privacy & cost"),
    ]
    .spacing(2);

    let body: Element<'_, Message> = match state.tab {
        Tab::Providers => {
            let default = labeled_pick_list(
                "Default".into(),
                ProviderKind::ALL.to_vec(),
                Some(state.settings.default_provider),
                |k| msg(AiSettingsMsg::DefaultProvider(k)),
                iced::Length::Fixed(220.0),
            );
            let test = button(text(if state.testing { "Testing…" } else { "Test default" }).size(11))
                .on_press_maybe((!state.testing).then(|| msg(AiSettingsMsg::Test)))
                .padding([3, 8]);
            scrollable(
                column![
                    row![default, test].spacing(10).align_y(iced::Center),
                    status_line(state.test_result.as_ref()),
                    provider_section(state, ProviderKind::OpenRouter),
                    provider_section(state, ProviderKind::Ollama),
                    provider_section(state, ProviderKind::OpenAiCompatible),
                ]
                .spacing(10)
                .padding(iced::Padding::ZERO.right(12)),
            )
            .height(Fill)
            .into()
        }
        Tab::Features => {
            let mut rows = column![
                text("Leave the model empty to use the provider's default model. Features marked ◉ send images and need a vision model.")
                    .size(11),
                row![
                    text("Feature").size(11).width(170),
                    text("Provider").size(11).width(170),
                    text("Model").size(11).width(Fill),
                ]
                .spacing(8),
            ]
            .spacing(8);
            for feature in AiFeature::ALL {
                let route: FeatureRoute = state.settings.features.get(&feature).cloned().unwrap_or_default();
                let choice = route
                    .provider
                    .map(ProviderChoice::Fixed)
                    .unwrap_or(ProviderChoice::Default);
                let effective = route.provider.unwrap_or(state.settings.default_provider);
                let label = if feature.needs_vision() {
                    format!("◉ {}", feature.label())
                } else {
                    feature.label().to_string()
                };
                let model = state
                    .settings
                    .features
                    .get(&feature)
                    .map(|r| r.model.as_str())
                    .unwrap_or("");
                rows = rows.push(
                    row![
                        text(label).size(12).width(170),
                        pick_list(Some(choice), ProviderChoice::ALL.to_vec(), |c| c.to_string())
                            .on_select(move |c| msg(AiSettingsMsg::FeatureProvider(feature, c)))
                            .text_size(12)
                            .padding([3, 6])
                            .width(170),
                        model_picker(
                            state,
                            effective,
                            model,
                            move |v| msg(AiSettingsMsg::FeatureModel(feature, v)),
                            move |v| msg(AiSettingsMsg::FeatureModel(feature, v)),
                            state.settings.provider(effective).default_model.as_str(),
                        ),
                    ]
                    .spacing(8)
                    .align_y(iced::Center),
                );
            }
            scrollable(rows.padding(iced::Padding::ZERO.right(12)))
                .height(Fill)
                .into()
        }
        Tab::Privacy => column![
            checkbox(state.settings.local_only)
                .label("Only use local models (Ollama or a localhost server) — no drawing data leaves this computer")
                .on_toggle(|v| msg(AiSettingsMsg::LocalOnly(v)))
                .size(14),
            checkbox(state.settings.confirm_before_send)
                .label("Show a token and cost estimate and ask before each hosted request")
                .on_toggle(|v| msg(AiSettingsMsg::ConfirmBeforeSend(v)))
                .size(14),
            labeled_field(
                "Timeout (s)".into(),
                &state.timeout_text,
                |v| msg(AiSettingsMsg::Timeout(v)),
                100.0
            ),
            labeled_field(
                "Max reply tokens".into(),
                &state.tokens_text,
                |v| msg(AiSettingsMsg::MaxTokens(v)),
                100.0
            ),
            text("API keys are stored in ai_keys.json in your Open CAD Studio settings folder, separate from ai_settings.json. Set OPENROUTER_API_KEY or OCS_OPENAI_API_KEY to keep keys out of the settings folder entirely.")
                .size(11),
        ]
        .spacing(12)
        .into(),
    };

    let error: Element<'_, Message> = match &state.error {
        Some(m) => text(m.as_str()).size(12).style(text::danger).into(),
        None => Space::new().height(0).into(),
    };
    let buttons = row![
        Space::new().width(Fill),
        dialog_button("OK", msg(AiSettingsMsg::Ok), true),
        dialog_button("Cancel", Message::CloseModal, false),
    ]
    .spacing(8)
    .align_y(iced::Center);
    container(
        column![
            tabs,
            hdivider(sizing.width),
            body,
            error,
            hdivider(sizing.width),
            buttons
        ]
        .spacing(10),
    )
    .padding(16)
    .width(sizing.width)
    .height(sizing.height)
    .into()
}
