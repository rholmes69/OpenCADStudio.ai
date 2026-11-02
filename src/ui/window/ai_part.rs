//! Text to part dialog (AIPART): describe a part, review its parameters,
//! insert the solid; reopen a part made this way to change its sizes.

use crate::ai::part_gen::{extents, instances, BodyOp, Part};
use crate::ai::ChatResponse;
use crate::app::Message;
use crate::ui::style::form::{dialog_button, dialog_button_styled_opt, field_style, hdivider};
use iced::widget::{button, column, container, row, scrollable, text, text_input};
use iced::{Element, Fill};

#[derive(Debug, Clone)]
pub enum AiPartMsg {
    Prompt(String),
    Generate,
    Reply(Result<ChatResponse, String>),
    Param(usize, String),
    Insert,
    OpenSettings,
}

pub(crate) struct State {
    /// The solid being edited, when the dialog opened on an AIPART solid.
    pub editing: Option<codec::Handle>,
    pub origin: Option<[f64; 3]>,
    pub prompt: String,
    pub part: Option<Part>,
    pub fields: Vec<String>,
    pub field_errors: Vec<Option<String>>,
    pub busy: bool,
    pub status: Option<Result<String, String>>,
    pub estimate: String,
    pub confirm_armed: bool,
    pub retries: u8,
}

impl State {
    pub fn new() -> Self {
        Self {
            editing: None,
            origin: None,
            prompt: String::new(),
            part: None,
            fields: Vec::new(),
            field_errors: Vec::new(),
            busy: false,
            status: None,
            estimate: String::new(),
            confirm_armed: false,
            retries: 0,
        }
    }

    pub fn set_part(&mut self, part: Part) {
        self.fields = part
            .params
            .iter()
            .map(|p| match &p.expr {
                serde_json::Value::String(s) => s.clone(),
                other => trim_num(other.as_f64().unwrap_or(p.value)),
            })
            .collect();
        self.field_errors = vec![None; self.fields.len()];
        self.part = Some(part);
    }
}

fn trim_num(x: f64) -> String {
    let s = format!("{x:.6}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

fn msg(m: AiPartMsg) -> Message {
    Message::AiPart(m)
}

pub(crate) fn view(state: &State, sizing: crate::ui::modal::ModalSizing) -> Element<'_, Message> {
    let header = row![
        text(if state.editing.is_some() {
            "Editing a part made with AIPART — change its parameters and rebuild."
        } else {
            "Describe a part, e.g. \"M8 hex bolt, 40 mm long\" or \"L-bracket 3 mm sheet, 50×50, two 6 mm holes per leg\"."
        })
        .size(12)
        .width(Fill),
        button(text("Settings…").size(11))
            .on_press(msg(AiPartMsg::OpenSettings))
            .padding([4, 10]),
    ]
    .spacing(8)
    .align_y(iced::Center);

    let prompt = row![
        text_input("Describe the part…", &state.prompt)
            .on_input(|v| msg(AiPartMsg::Prompt(v)))
            .on_submit(msg(AiPartMsg::Generate))
            .style(field_style)
            .size(13)
            .padding(8)
            .width(Fill),
        dialog_button_styled_opt(
            if state.confirm_armed {
                "Confirm & generate"
            } else if state.part.is_some() {
                "Generate again"
            } else {
                "Generate"
            },
            (!state.busy && !state.prompt.trim().is_empty()).then(|| msg(AiPartMsg::Generate)),
            if state.part.is_some() {
                button::secondary
            } else {
                button::primary
            }
        ),
    ]
    .spacing(8)
    .align_y(iced::Center);

    let mut body = column![].spacing(8);
    match &state.part {
        None => {
            body = body.push(
                text("The model writes a parametric definition — named sizes, plus boxes, cylinders, polygon prisms, extrusions and revolved sections that are joined, cut or intersected. You review the parameters here before anything is drawn, and the definition is stored on the solid so AIPART can reopen it later.")
                    .size(12),
            );
        }
        Some(part) => {
            body = body.push(
                text(format!(
                    "{}  ({})",
                    part.name,
                    if part.units.is_empty() {
                        "drawing units"
                    } else {
                        &part.units
                    }
                ))
                .size(14),
            );
            if !part.notes.is_empty() {
                body = body.push(text(format!("Notes: {}", part.notes)).size(11));
            }
            match instances(part) {
                Ok(insts) => {
                    let (lo, hi) = extents(&insts);
                    let count = |op: BodyOp| insts.iter().filter(|i| i.op == op).count();
                    body = body.push(
                        text(format!(
                            "Size about {} × {} × {}; {} added, {} cut, {} intersected bodies",
                            trim_num(hi[0] - lo[0]),
                            trim_num(hi[1] - lo[1]),
                            trim_num(hi[2] - lo[2]),
                            count(BodyOp::Add),
                            count(BodyOp::Cut),
                            count(BodyOp::Intersect)
                        ))
                        .size(12),
                    );
                }
                Err(e) => body = body.push(text(e).size(12).style(text::danger)),
            }
            body = body
                .push(text("Parameters (a number or an expression of other parameters):").size(12));
            for (n, p) in part.params.iter().enumerate() {
                let mut r = row![
                    text(p.name.as_str()).size(12).width(110),
                    text_input("", state.fields.get(n).map(String::as_str).unwrap_or(""))
                        .on_input(move |v| msg(AiPartMsg::Param(n, v)))
                        .style(field_style)
                        .size(12)
                        .width(160),
                    text(format!("= {}", trim_num(p.value))).size(11).width(90),
                ]
                .spacing(8)
                .align_y(iced::Center);
                if let Some(Some(e)) = state.field_errors.get(n) {
                    r = r.push(text(e.as_str()).size(11).style(text::danger));
                }
                body = body.push(r);
            }
        }
    }
    let body = container(scrollable(body.padding(iced::Padding::ZERO.right(12))).height(Fill))
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
    let ready = state.part.as_ref().is_some_and(|p| instances(p).is_ok())
        && state.field_errors.iter().all(Option::is_none);
    let footer = row![
        text(state.estimate.as_str()).size(11).width(Fill),
        dialog_button_styled_opt(
            if state.editing.is_some() {
                "Rebuild (one undo step)"
            } else {
                "Insert (one undo step)"
            },
            (!state.busy && ready).then(|| msg(AiPartMsg::Insert)),
            button::primary
        ),
        dialog_button("Close", Message::CloseModal, false),
    ]
    .spacing(8)
    .align_y(iced::Center);

    container(column![header, hdivider(sizing.width), prompt, body, info, footer].spacing(10))
        .padding(16)
        .width(sizing.width)
        .height(sizing.height)
        .into()
}
