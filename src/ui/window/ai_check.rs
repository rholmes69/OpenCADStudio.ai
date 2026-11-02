//! Drawing checker dialog (AICHECK): rule findings against a drafting
//! standard, an optional AI review, and Show / Fix per finding.

use crate::ai::drawing_check::{Finding, Fix, Severity, Standard, StandardKind};
use crate::ai::ChatResponse;
use crate::app::Message;
use crate::ui::style::form::{
    dialog_button, dialog_button_styled_opt, hdivider, labeled_pick_list,
};
use iced::widget::{button, column, container, row, scrollable, text, Space};
use iced::{Element, Fill, Theme};

#[derive(Debug, Clone)]
pub enum AiCheckMsg {
    Standard(StandardKind),
    LoadCompany,
    CompanyLoaded(Result<(String, Standard), String>),
    Run,
    Review,
    ReviewReply(Result<ChatResponse, String>),
    Show(usize),
    Fix(usize),
    OpenSettings,
}

pub(crate) struct State {
    pub standard: StandardKind,
    pub company: Option<Standard>,
    pub findings: Vec<Finding>,
    pub ran: bool,
    pub objects_checked: usize,
    pub busy: bool,
    pub ai_summary: String,
    pub status: Option<Result<String, String>>,
    pub estimate: String,
    pub confirm_armed: bool,
}

impl State {
    pub fn new(company: Option<Standard>) -> Self {
        Self {
            standard: if company.is_some() {
                StandardKind::Company
            } else {
                StandardKind::Iso
            },
            company,
            findings: Vec::new(),
            ran: false,
            objects_checked: 0,
            busy: false,
            ai_summary: String::new(),
            status: None,
            estimate: String::new(),
            confirm_armed: false,
        }
    }

    pub fn standard(&self) -> Standard {
        Standard::for_kind(self.standard, self.company.as_ref())
    }
}

fn msg(m: AiCheckMsg) -> Message {
    Message::AiCheck(m)
}

fn severity_style(s: Severity) -> fn(&Theme) -> text::Style {
    match s {
        Severity::Error => text::danger,
        Severity::Warning => text::warning,
        Severity::Info => text::secondary,
    }
}

pub(crate) fn view(state: &State, sizing: crate::ui::modal::ModalSizing) -> Element<'_, Message> {
    let mut top = row![labeled_pick_list(
        "Standard".into(),
        StandardKind::ALL.to_vec(),
        Some(state.standard),
        |k| msg(AiCheckMsg::Standard(k)),
        iced::Length::Fixed(230.0)
    )]
    .spacing(8)
    .align_y(iced::Center);
    if state.standard == StandardKind::Company {
        top = top.push(
            button(text("Load standard file…").size(11))
                .on_press(msg(AiCheckMsg::LoadCompany))
                .padding([4, 10]),
        );
        top = top.push(
            text(
                state
                    .company
                    .as_ref()
                    .map(|c| c.name.clone())
                    .unwrap_or_else(|| "none loaded — ISO rules apply".into()),
            )
            .size(11),
        );
    }
    top = top.push(Space::new().width(Fill));
    top = top.push(
        button(text("Settings…").size(11))
            .on_press(msg(AiCheckMsg::OpenSettings))
            .padding([4, 10]),
    );

    let mut list = column![].spacing(6);
    if !state.ran {
        list = list.push(text("Run checks to test this drawing against the chosen standard: layer use, ByLayer properties, layer naming, text heights, dimension text and associativity, overlapping annotation, duplicate and zero-size objects. AI review then looks for missing or duplicated dimensions and inconsistent notes.").size(12));
    } else if state.findings.is_empty() {
        list = list.push(
            text(format!(
                "No findings in {} object(s).",
                state.objects_checked
            ))
            .size(12)
            .style(text::success),
        );
    }
    for (n, f) in state.findings.iter().enumerate() {
        let tag = if f.from_ai { "AI" } else { f.rule.as_str() };
        let mut actions = row![].spacing(4);
        if !f.handles.is_empty() {
            actions = actions.push(
                button(text("Show").size(11))
                    .on_press_maybe((!state.busy).then(|| msg(AiCheckMsg::Show(n))))
                    .padding([2, 8]),
            );
        }
        if f.fix == Some(Fix::Erase) {
            actions = actions.push(
                button(text("Erase").size(11))
                    .on_press_maybe((!state.busy).then(|| msg(AiCheckMsg::Fix(n))))
                    .style(button::danger)
                    .padding([2, 8]),
            );
        }
        list = list.push(
            row![
                text(f.severity.label())
                    .size(11)
                    .style(severity_style(f.severity))
                    .width(60),
                text(tag.to_string()).size(11).width(100),
                text(f.message.as_str()).size(12).width(Fill),
                actions,
            ]
            .spacing(8)
            .align_y(iced::Center),
        );
    }
    let list = container(scrollable(list.padding(iced::Padding::ZERO.right(12))).height(Fill))
        .style(container::bordered_box)
        .padding(10)
        .width(Fill)
        .height(Fill);

    let mut info = column![].spacing(4);
    if !state.ai_summary.is_empty() {
        info = info.push(text(format!("AI review: {}", state.ai_summary)).size(12));
    }
    match &state.status {
        Some(Ok(s)) => info = info.push(text(s.as_str()).size(12).style(text::success)),
        Some(Err(e)) => info = info.push(text(e.as_str()).size(12).style(text::danger)),
        None => {}
    }
    if state.busy {
        info = info.push(text("Waiting for the AI review…").size(12));
    }

    let review_label = if state.confirm_armed {
        "Confirm & review"
    } else {
        "AI review"
    };
    let footer = row![
        text(state.estimate.as_str()).size(11).width(Fill),
        dialog_button_styled_opt(
            "Run checks",
            (!state.busy).then(|| msg(AiCheckMsg::Run)),
            button::primary
        ),
        dialog_button_styled_opt(
            review_label,
            (!state.busy && state.ran).then(|| msg(AiCheckMsg::Review)),
            button::secondary
        ),
        dialog_button("Close", Message::CloseModal, false),
    ]
    .spacing(8)
    .align_y(iced::Center);

    container(column![top, hdivider(sizing.width), list, info, footer].spacing(10))
        .padding(16)
        .width(sizing.width)
        .height(sizing.height)
        .into()
}
