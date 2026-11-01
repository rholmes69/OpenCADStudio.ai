//! AI Assistant dialog: the natural-language command bar (AICMD) and chat
//! with the drawing (AICHAT) share one conversation window.

use crate::ai::command_plan::{describe_step, Plan};
use crate::ai::{ChatMessage, ChatResponse};
use crate::app::Message;
use crate::ui::style::form::{dialog_button, dialog_button_styled_opt, field_style, hdivider};
use iced::widget::{button, column, container, row, scrollable, text, text_input, Space};
use iced::{Element, Fill, Theme};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Describe a change; get a plan to review and apply.
    Command,
    /// Ask about the drawing; the model queries it read-only.
    Chat,
}

impl Mode {
    pub fn feature(self) -> crate::ai::AiFeature {
        match self {
            Mode::Command => crate::ai::AiFeature::CommandBar,
            Mode::Chat => crate::ai::AiFeature::DrawingChat,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    User,
    Assistant,
    Tool,
    Info,
    Error,
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub kind: EntryKind,
    pub text: String,
}

#[derive(Debug, Clone)]
pub enum AiAssistMsg {
    Mode(Mode),
    Input(String),
    Send,
    Reply(Result<ChatResponse, String>),
    Apply,
    Discard,
    Clear,
    OpenSettings,
}

pub(crate) struct State {
    pub mode: Mode,
    pub input: String,
    pub transcript: Vec<Entry>,
    /// Conversation sent to the model (system prompt excluded: it is rebuilt
    /// with fresh drawing context on every request).
    pub history: Vec<ChatMessage>,
    pub busy: bool,
    /// Read-only queries made for the current question (AICHAT).
    pub tool_calls: usize,
    /// Automatic retries after an unusable reply (one per question).
    pub retries: u8,
    pub plan: Option<Plan>,
    /// Shown under the input: provider, model, token / cost estimate.
    pub estimate: String,
    /// The last system prompt sent (or built on open), reused for the
    /// per-keystroke estimate so typing never re-reads a large drawing.
    pub prompt_cache: String,
    /// The estimate was shown; the next Send goes out.
    pub confirm_armed: bool,
    pub spent_usd: f64,
    pub tokens_in: u64,
    pub tokens_out: u64,
}

impl State {
    pub fn new(mode: Mode) -> Self {
        Self {
            mode,
            input: String::new(),
            transcript: Vec::new(),
            history: Vec::new(),
            busy: false,
            tool_calls: 0,
            retries: 0,
            plan: None,
            estimate: String::new(),
            prompt_cache: String::new(),
            confirm_armed: false,
            spent_usd: 0.0,
            tokens_in: 0,
            tokens_out: 0,
        }
    }

    pub fn push(&mut self, kind: EntryKind, text: impl Into<String>) {
        self.transcript.push(Entry {
            kind,
            text: text.into(),
        });
    }
}

fn msg(m: AiAssistMsg) -> Message {
    Message::AiAssist(m)
}

fn mode_button(state: &State, mode: Mode, label: &'static str) -> Element<'static, Message> {
    let active = state.mode == mode;
    button(text(label).size(11))
        .on_press_maybe((!active && !state.busy).then(|| msg(AiAssistMsg::Mode(mode))))
        .style(if active {
            button::primary
        } else {
            button::secondary
        })
        .padding([4, 10])
        .into()
}

fn entry_view(entry: &Entry) -> Element<'_, Message> {
    let (who, style): (&str, fn(&Theme) -> text::Style) = match entry.kind {
        EntryKind::User => ("You", text::primary),
        EntryKind::Assistant => ("Assistant", text::base),
        EntryKind::Tool => ("Query", text::secondary),
        EntryKind::Info => ("•", text::success),
        EntryKind::Error => ("!", text::danger),
    };
    row![
        text(who).size(11).style(style).width(70),
        text(entry.text.as_str()).size(12).width(Fill),
    ]
    .spacing(8)
    .into()
}

pub(crate) fn view(state: &State, sizing: crate::ui::modal::ModalSizing) -> Element<'_, Message> {
    let header = row![
        mode_button(state, Mode::Command, "Command (AICMD)"),
        mode_button(state, Mode::Chat, "Ask the drawing (AICHAT)"),
        Space::new().width(Fill),
        button(text("Settings…").size(11))
            .on_press(msg(AiAssistMsg::OpenSettings))
            .padding([4, 10]),
        button(text("New conversation").size(11))
            .on_press_maybe((!state.busy).then(|| msg(AiAssistMsg::Clear)))
            .padding([4, 10]),
    ]
    .spacing(6)
    .align_y(iced::Center);

    let mut log = column![].spacing(8);
    if state.transcript.is_empty() {
        let hint = match state.mode {
            Mode::Command => "Describe what to draw or change, e.g. \"draw a 40×20 rectangle with 3 mm fillets at the origin, then array it 5 times along X at 50 spacing\". You will see the plan before anything changes.",
            Mode::Chat => "Ask about this drawing, e.g. \"how many blocks are unused?\" or \"what is the total length of polylines on layer CUTTING?\". Answers come from queries against the drawing.",
        };
        log = log.push(text(hint).size(12));
    }
    for entry in &state.transcript {
        log = log.push(entry_view(entry));
    }
    if state.busy {
        log = log.push(text("Waiting for the model…").size(12));
    }
    let log = container(
        scrollable(log.padding(iced::Padding::ZERO.right(12)))
            .anchor_bottom()
            .height(Fill),
    )
    .style(container::bordered_box)
    .padding(10)
    .width(Fill)
    .height(Fill);

    let plan_panel: Element<'_, Message> = match &state.plan {
        Some(plan) => {
            let mut steps = column![text(if plan.summary.is_empty() {
                "Proposed plan".to_string()
            } else {
                format!("Proposed plan: {}", plan.summary)
            })
            .size(12)]
            .spacing(4);
            for (n, step) in plan.steps.iter().enumerate() {
                steps = steps.push(text(format!("{}. {}", n + 1, describe_step(step))).size(12));
            }
            container(
                column![
                    scrollable(steps).height(160),
                    row![
                        Space::new().width(Fill),
                        dialog_button_styled_opt(
                            "Discard",
                            (!state.busy).then(|| msg(AiAssistMsg::Discard)),
                            button::secondary
                        ),
                        dialog_button_styled_opt(
                            "Apply (one undo step)",
                            (!state.busy).then(|| msg(AiAssistMsg::Apply)),
                            button::primary
                        ),
                    ]
                    .spacing(8),
                ]
                .spacing(8),
            )
            .style(container::bordered_box)
            .padding(10)
            .width(Fill)
            .into()
        }
        None => Space::new().height(0).into(),
    };

    let placeholder = match state.mode {
        Mode::Command => "Describe a change to the drawing…",
        Mode::Chat => "Ask a question about this drawing…",
    };
    let input = text_input(placeholder, &state.input)
        .on_input(|v| msg(AiAssistMsg::Input(v)))
        .on_submit(msg(AiAssistMsg::Send))
        .style(field_style)
        .size(13)
        .padding(8)
        .width(Fill);
    let send_label = if state.confirm_armed {
        "Confirm & send"
    } else {
        "Send"
    };
    let send = dialog_button_styled_opt(
        send_label,
        (!state.busy && !state.input.trim().is_empty()).then(|| msg(AiAssistMsg::Send)),
        button::primary,
    );
    let mut footer_text = state.estimate.clone();
    if state.tokens_in + state.tokens_out > 0 {
        footer_text.push_str(&format!(
            "   |   this conversation: {} in / {} out tokens",
            state.tokens_in, state.tokens_out
        ));
        if state.spent_usd > 0.0 {
            footer_text.push_str(&format!(", ${:.4}", state.spent_usd));
        }
    }

    container(
        column![
            header,
            hdivider(sizing.width),
            log,
            plan_panel,
            row![input, send].spacing(8).align_y(iced::Center),
            row![
                text(footer_text).size(11).width(Fill),
                dialog_button("Close", Message::CloseModal, false),
            ]
            .spacing(8)
            .align_y(iced::Center),
        ]
        .spacing(10),
    )
    .padding(16)
    .width(sizing.width)
    .height(sizing.height)
    .into()
}
