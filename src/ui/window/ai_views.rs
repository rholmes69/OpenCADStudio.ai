//! Drawing views dialog (AIVIEWS): choose views, hidden lines and a
//! dimension scheme for the selected solid, preview the sheet, insert.

use crate::ai::drawing_views::{layout, Dim, Shape2, View, ViewKind};
use crate::ai::ChatResponse;
use crate::app::Message;
use crate::ui::style::form::{dialog_button, dialog_button_styled_opt, hdivider};
use iced::mouse;
use iced::widget::{button, canvas, checkbox, column, container, pick_list, row, text};
use iced::{Color, Element, Fill, Point, Rectangle, Theme};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DimMode {
    None,
    Basic,
    Ai,
}

impl DimMode {
    pub const ALL: [DimMode; 3] = [DimMode::None, DimMode::Basic, DimMode::Ai];
}

impl std::fmt::Display for DimMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            DimMode::None => "No dimensions",
            DimMode::Basic => "Basic (overall sizes, holes)",
            DimMode::Ai => "AI dimension scheme",
        })
    }
}

#[derive(Debug, Clone)]
pub enum AiViewsMsg {
    Toggle(ViewKind, bool),
    Hidden(bool),
    Dims(DimMode),
    Standard(String),
    Suggest,
    Reply(Result<ChatResponse, String>),
    Insert,
    OpenSettings,
}

pub const STANDARDS: [&str; 2] = ["ISO 129-1", "ASME Y14.5"];

pub(crate) struct State {
    /// The solid the views are of (kept for a later "update views").
    #[allow(dead_code)]
    pub solid: codec::Handle,
    pub data: crate::ai::drawing_views::SolidData,
    /// Kernel silhouette segments per view kind.
    pub silhouettes: Vec<(ViewKind, Vec<[[f64; 3]; 2]>)>,
    pub enabled: Vec<ViewKind>,
    pub hidden: bool,
    pub dim_mode: DimMode,
    pub standard: String,
    pub views: Vec<View>,
    pub ai_dims: Option<Vec<Dim>>,
    pub busy: bool,
    pub status: Option<Result<String, String>>,
    pub estimate: String,
    pub confirm_armed: bool,
}

fn msg(m: AiViewsMsg) -> Message {
    Message::AiViews(m)
}

struct Preview<'a> {
    views: &'a [View],
}

impl canvas::Program<Message> for Preview<'_> {
    type State = ();

    fn draw(
        &self,
        _s: &(),
        renderer: &iced::Renderer,
        theme: &Theme,
        bounds: Rectangle,
        _c: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        if self.views.is_empty() {
            return vec![frame.into_geometry()];
        }
        let size = self
            .views
            .iter()
            .map(|v| v.width().max(v.height()))
            .fold(0.0, f64::max);
        let offsets = layout(self.views, [0.0, 0.0], size * 0.3);
        let (mut lo, mut hi) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
        for (v, o) in self.views.iter().zip(&offsets) {
            lo = [lo[0].min(o[0]), lo[1].min(o[1])];
            hi = [hi[0].max(o[0] + v.width()), hi[1].max(o[1] + v.height())];
        }
        let pad = 20.0;
        let k = ((bounds.width as f64 - 2.0 * pad) / (hi[0] - lo[0]).max(1e-9))
            .min((bounds.height as f64 - 2.0 * pad) / (hi[1] - lo[1]).max(1e-9));
        let map = |v: &View, o: [f64; 2], p: [f64; 2]| {
            Point::new(
                (pad + (p[0] - v.lo[0] + o[0] - lo[0]) * k) as f32,
                (bounds.height as f64 - pad - (p[1] - v.lo[1] + o[1] - lo[1]) * k) as f32,
            )
        };
        let ink = theme.palette().background.base.text;
        let faint = Color { a: 0.45, ..ink };
        for (v, o) in self.views.iter().zip(&offsets) {
            for (shapes, color) in [(&v.visible, ink), (&v.hidden, faint)] {
                for s in shapes {
                    let path = canvas::Path::new(|b| match s {
                        Shape2::Line { a, b: e } => {
                            b.move_to(map(v, *o, *a));
                            b.line_to(map(v, *o, *e));
                        }
                        Shape2::Polyline { pts } => {
                            if let Some(f) = pts.first() {
                                b.move_to(map(v, *o, *f));
                                for p in &pts[1..] {
                                    b.line_to(map(v, *o, *p));
                                }
                            }
                        }
                        Shape2::Circle { c, r } | Shape2::Arc { c, r, .. } => {
                            let (a0, a1) = match s {
                                Shape2::Arc { start, end, .. } => (*start, *end),
                                _ => (0.0, 360.0),
                            };
                            let n = 48;
                            for i in 0..=n {
                                let t = (a0 + (a1 - a0) * i as f64 / n as f64).to_radians();
                                let p = map(v, *o, [c[0] + r * t.cos(), c[1] + r * t.sin()]);
                                if i == 0 {
                                    b.move_to(p)
                                } else {
                                    b.line_to(p)
                                }
                            }
                        }
                    });
                    frame.stroke(
                        &path,
                        canvas::Stroke::default().with_color(color).with_width(1.2),
                    );
                }
            }
            frame.fill_text(canvas::Text {
                content: v.kind.label().to_string(),
                position: Point::new(map(v, *o, v.lo).x, map(v, *o, v.lo).y + 6.0),
                color: faint,
                size: 11.0.into(),
                ..Default::default()
            });
        }
        vec![frame.into_geometry()]
    }
}

pub(crate) fn view(state: &State, sizing: crate::ui::modal::ModalSizing) -> Element<'_, Message> {
    let mut toggles = row![].spacing(12).align_y(iced::Center);
    for k in ViewKind::ALL {
        toggles = toggles.push(
            checkbox(state.enabled.contains(&k))
                .label(k.label().to_string())
                .on_toggle(move |v| msg(AiViewsMsg::Toggle(k, v)))
                .size(14),
        );
    }
    toggles = toggles.push(
        checkbox(state.hidden)
            .label("Hidden lines")
            .on_toggle(|v| msg(AiViewsMsg::Hidden(v)))
            .size(14),
    );
    let mut dims = row![
        text("Dimensions").size(11).width(80),
        pick_list(Some(state.dim_mode), DimMode::ALL.to_vec(), |m| m
            .to_string())
        .on_select(|m| msg(AiViewsMsg::Dims(m)))
        .text_size(12)
        .padding([3, 6])
        .width(240),
    ]
    .spacing(8)
    .align_y(iced::Center);
    if state.dim_mode == DimMode::Ai {
        dims = dims.push(
            pick_list(
                Some(state.standard.clone()),
                STANDARDS.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
                |s| s.clone(),
            )
            .on_select(|s| msg(AiViewsMsg::Standard(s)))
            .text_size(12)
            .padding([3, 6])
            .width(140),
        );
        dims = dims.push(dialog_button_styled_opt(
            if state.confirm_armed {
                "Confirm & suggest"
            } else if state.ai_dims.is_some() {
                "Suggest again"
            } else {
                "Suggest"
            },
            (!state.busy).then(|| msg(AiViewsMsg::Suggest)),
            button::secondary,
        ));
    }
    dims = dims.push(iced::widget::Space::new().width(Fill));
    dims = dims.push(
        button(text("Settings…").size(11))
            .on_press(msg(AiViewsMsg::OpenSettings))
            .padding([4, 10]),
    );

    let preview = container(
        canvas(Preview {
            views: &state.views,
        })
        .width(Fill)
        .height(Fill),
    )
    .style(container::bordered_box)
    .padding(4)
    .width(Fill)
    .height(Fill);

    let mut info = column![].spacing(4);
    let lines: usize = state.views.iter().map(|v| v.visible.len()).sum();
    let hidden: usize = state.views.iter().map(|v| v.hidden.len()).sum();
    info = info.push(
        text(format!(
            "{} view(s): {lines} visible and {hidden} hidden lines/curves",
            state.views.len()
        ))
        .size(12),
    );
    match &state.status {
        Some(Ok(s)) => info = info.push(text(s.as_str()).size(12).style(text::success)),
        Some(Err(e)) => info = info.push(text(e.as_str()).size(12).style(text::danger)),
        None => {}
    }
    if state.busy {
        info = info.push(text("Waiting for the model…").size(12));
    }
    let ready =
        !state.views.is_empty() && (state.dim_mode != DimMode::Ai || state.ai_dims.is_some());
    let footer = row![
        text(state.estimate.as_str()).size(11).width(Fill),
        dialog_button_styled_opt(
            "Insert (one undo step)",
            (!state.busy && ready).then(|| msg(AiViewsMsg::Insert)),
            button::primary
        ),
        dialog_button("Close", Message::CloseModal, false),
    ]
    .spacing(8)
    .align_y(iced::Center);

    container(column![toggles, dims, hdivider(sizing.width), preview, info, footer].spacing(10))
        .padding(16)
        .width(sizing.width)
        .height(sizing.height)
        .into()
}
