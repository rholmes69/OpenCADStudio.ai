//! Constraint inference dialog (AICONSTRAIN): rule-based constraints from
//! Auto Constrain plus checked AI suggestions for design intent and a
//! dimension scheme, each with its own tick box.

use crate::ai::constraint_suggest::{describe, Prim, Rejected, Suggestion};
use crate::ai::ChatResponse;
use crate::app::Message;
use crate::ui::style::form::{dialog_button, dialog_button_styled_opt, hdivider};
use iced::widget::{button, checkbox, column, container, row, scrollable, text, Space};
use iced::{Element, Fill};

#[derive(Debug, Clone)]
pub enum AiConstrainMsg {
    Analyze,
    Reply(Result<ChatResponse, String>),
    Toggle(usize, bool),
    IncludeAuto(bool),
    All(bool),
    Apply,
    OpenSettings,
}

pub(crate) struct State {
    pub handles: Vec<codec::Handle>,
    pub prims: Vec<Prim>,
    /// Auto Constrain's rule-based relations for the same objects.
    pub auto: Vec<(
        crate::scene::parametric_constraints::ConstraintKind,
        Vec<crate::scene::parametric_constraints::ParametricRef>,
    )>,
    pub include_auto: bool,
    pub intent: String,
    pub suggestions: Vec<Suggestion>,
    pub accepted: Vec<bool>,
    pub rejected: Vec<Rejected>,
    pub busy: bool,
    pub status: Option<Result<String, String>>,
    pub estimate: String,
    pub confirm_armed: bool,
}

fn msg(m: AiConstrainMsg) -> Message {
    Message::AiConstrain(m)
}

pub(crate) fn view(state: &State, sizing: crate::ui::modal::ModalSizing) -> Element<'_, Message> {
    let header = row![
        text(format!(
            "{} object(s), {} sketch primitive(s)",
            state.handles.len(),
            state.prims.len()
        ))
        .size(12),
        Space::new().width(Fill),
        button(text("Settings…").size(11))
            .on_press(msg(AiConstrainMsg::OpenSettings))
            .padding([4, 10]),
    ]
    .align_y(iced::Center);

    let mut list = column![checkbox(state.include_auto)
        .label(format!(
            "Rule-based geometric constraints from Auto Constrain ({})",
            state.auto.len()
        ))
        .on_toggle(|v| msg(AiConstrainMsg::IncludeAuto(v)))
        .size(14)]
    .spacing(6);
    if !state.intent.is_empty() {
        list = list.push(text(format!("Design intent: {}", state.intent)).size(12));
    }
    if state.suggestions.is_empty() && !state.busy {
        list = list.push(
            text("Press Suggest to have the AI propose design-intent relations (equal hole sizes, symmetry, concentric features) and a dimension scheme. Every suggestion is checked against the geometry first, and dimensions keep the current sizes, so applying never moves anything.")
                .size(12),
        );
    } else if !state.suggestions.is_empty() {
        list = list.push(
            row![
                text("AI suggestions").size(12),
                Space::new().width(Fill),
                button(text("All").size(11))
                    .on_press(msg(AiConstrainMsg::All(true)))
                    .padding([2, 8]),
                button(text("None").size(11))
                    .on_press(msg(AiConstrainMsg::All(false)))
                    .padding([2, 8]),
            ]
            .spacing(6)
            .align_y(iced::Center),
        );
    }
    for (n, s) in state.suggestions.iter().enumerate() {
        list = list.push(
            checkbox(state.accepted.get(n).copied().unwrap_or(false))
                .label(describe(s))
                .on_toggle(move |v| msg(AiConstrainMsg::Toggle(n, v)))
                .size(14),
        );
    }
    if !state.rejected.is_empty() {
        list = list.push(
            text(format!(
                "{} proposal(s) were dropped because the geometry does not support them:",
                state.rejected.len()
            ))
            .size(11)
            .style(text::secondary),
        );
        for r in state.rejected.iter().take(20) {
            list = list.push(
                text(format!("   {} — {}", r.text, r.reason))
                    .size(11)
                    .style(text::secondary),
            );
        }
    }
    let list = container(scrollable(list.padding(iced::Padding::ZERO.right(12))).height(Fill))
        .style(container::bordered_box)
        .padding(10)
        .width(Fill)
        .height(Fill);

    let mut info = column![].spacing(4);
    match &state.status {
        Some(Ok(s)) => info = info.push(text(s.as_str()).size(12).style(text::success)),
        Some(Err(e)) => info = info.push(text(e.as_str()).size(12).style(text::danger)),
        None => {}
    }
    if state.busy {
        info = info.push(text("Waiting for the model…").size(12));
    }
    let chosen = state.accepted.iter().filter(|a| **a).count()
        + if state.include_auto {
            state.auto.len()
        } else {
            0
        };
    let footer = row![
        text(state.estimate.as_str()).size(11).width(Fill),
        dialog_button_styled_opt(
            if state.confirm_armed {
                "Confirm & suggest"
            } else {
                "Suggest"
            },
            (!state.busy && !state.prims.is_empty()).then(|| msg(AiConstrainMsg::Analyze)),
            button::secondary
        ),
        dialog_button_styled_opt(
            if chosen == 0 {
                "Apply".to_string()
            } else {
                format!("Apply {chosen}")
            },
            (!state.busy && chosen > 0).then(|| msg(AiConstrainMsg::Apply)),
            button::primary
        ),
        dialog_button("Close", Message::CloseModal, false),
    ]
    .spacing(8)
    .align_y(iced::Center);

    container(column![header, hdivider(sizing.width), list, info, footer].spacing(10))
        .padding(16)
        .width(sizing.width)
        .height(sizing.height)
        .into()
}
