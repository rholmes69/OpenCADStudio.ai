//! Floor-plan assistant dialog (AIPLAN): get a plan from a brief, a plan
//! image or selected lines; review it in a preview; name the rooms; insert
//! the layered plan, room schedule and optional 3D massing.

use crate::ai::floor_plan::{area_label, area_perimeter, centroid, extents, OpeningKind, Plan};
use crate::ai::ChatResponse;
use crate::app::Message;
use crate::ui::style::form::{
    dialog_button, dialog_button_styled_opt, field_style, hdivider, labeled_field,
};
use crate::ui::window::ai_image::Loaded;
use iced::mouse;
use iced::widget::{
    button, canvas, checkbox, column, container, row, scrollable, text, text_input,
};
use iced::{Color, Element, Fill, Point, Rectangle, Theme};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Brief,
    Image,
    Selection,
}

#[derive(Debug, Clone)]
pub enum AiPlanMsg {
    Source(Source),
    Brief(String),
    PickImage,
    ImagePicked(Result<std::path::PathBuf, String>),
    ImageLoaded(Result<Loaded, String>),
    Thickness(String),
    FromSelection,
    Generate,
    Reply(Result<ChatResponse, String>),
    RoomName(usize, String),
    Schedule(bool),
    Build3d(bool),
    WallHeight(String),
    Insert,
    OpenSettings,
}

pub(crate) struct State {
    pub source: Source,
    pub brief: String,
    pub image: Option<Loaded>,
    pub loading: bool,
    pub thickness: String,
    pub plan: Option<Plan>,
    pub schedule: bool,
    pub build_3d: bool,
    pub wall_height: String,
    /// Millimetres per unit of the plan (for area labels in the preview).
    pub mm_per_unit: Option<f64>,
    pub busy: bool,
    pub status: Option<Result<String, String>>,
    pub estimate: String,
    pub confirm_armed: bool,
    pub retries: u8,
}

fn msg(m: AiPlanMsg) -> Message {
    Message::AiPlan(m)
}

struct Preview<'a> {
    plan: &'a Plan,
    mm: Option<f64>,
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
        let (lo, hi) = extents(self.plan);
        let (w, h) = ((hi[0] - lo[0]).max(1e-9), (hi[1] - lo[1]).max(1e-9));
        let pad = 16.0;
        let k = ((bounds.width - 2.0 * pad) as f64 / w).min((bounds.height - 2.0 * pad) as f64 / h);
        let ox = (bounds.width as f64 - w * k) / 2.0;
        let oy = (bounds.height as f64 - h * k) / 2.0;
        let map = |p: [f64; 2]| {
            Point::new(
                (ox + (p[0] - lo[0]) * k) as f32,
                (bounds.height as f64 - oy - (p[1] - lo[1]) * k) as f32,
            )
        };
        let poly = |pts: &[[f64; 2]]| {
            canvas::Path::new(|b| {
                if let Some(f) = pts.first() {
                    b.move_to(map(*f));
                    for p in &pts[1..] {
                        b.line_to(map(*p));
                    }
                    b.close();
                }
            })
        };
        let palette = theme.palette();
        let ink = palette.background.base.text;
        let room_fill = Color {
            a: 0.08,
            ..palette.primary.base.color
        };
        for r in &self.plan.rooms {
            frame.fill(&poly(&r.points), room_fill);
        }
        for w in &self.plan.walls {
            frame.fill(
                &poly(&w.band(-w.thickness / 2.0, w.length() + w.thickness / 2.0)),
                ink,
            );
        }
        let door = Color::from_rgb(0.95, 0.55, 0.1);
        let glass = Color::from_rgb(0.2, 0.65, 0.95);
        for o in &self.plan.openings {
            let Some(w) = self.plan.walls.get(o.wall) else {
                continue;
            };
            let band = w.band(o.at - o.width / 2.0, o.at + o.width / 2.0);
            frame.fill(
                &poly(&band),
                if o.kind == OpeningKind::Door {
                    door
                } else {
                    glass
                },
            );
        }
        for r in &self.plan.rooms {
            let (a, _) = area_perimeter(&r.points);
            let c = map(centroid(&r.points));
            frame.fill_text(canvas::Text {
                content: format!("{}\n{}", r.name, area_label(a, self.mm)),
                position: c,
                color: ink,
                size: 12.0.into(),
                align_x: iced::widget::text::Alignment::Center,
                align_y: iced::alignment::Vertical::Center,
                ..Default::default()
            });
        }
        vec![frame.into_geometry()]
    }
}

pub(crate) fn view(state: &State, sizing: crate::ui::modal::ModalSizing) -> Element<'_, Message> {
    let tab = |s: Source, label: &'static str| {
        let active = state.source == s;
        button(text(label).size(11))
            .on_press_maybe((!active && !state.busy).then(|| msg(AiPlanMsg::Source(s))))
            .style(if active {
                button::primary
            } else {
                button::secondary
            })
            .padding([4, 10])
    };
    let header = row![
        tab(Source::Brief, "From a brief"),
        tab(Source::Image, "From a plan image"),
        tab(Source::Selection, "From selected lines"),
        iced::widget::Space::new().width(Fill),
        button(text("Settings…").size(11))
            .on_press(msg(AiPlanMsg::OpenSettings))
            .padding([4, 10]),
    ]
    .spacing(6)
    .align_y(iced::Center);

    let generate_label = if state.confirm_armed {
        "Confirm & generate"
    } else if state.plan.is_some() {
        "Generate again"
    } else {
        "Generate"
    };
    let source: Element<'_, Message> = match state.source {
        Source::Brief => row![
            text_input("e.g. 3-bedroom bungalow, 12 × 9 m, open kitchen-living, 2 bathrooms", &state.brief)
                .on_input(|v| msg(AiPlanMsg::Brief(v)))
                .on_submit(msg(AiPlanMsg::Generate))
                .style(field_style)
                .size(13)
                .padding(8)
                .width(Fill),
            dialog_button_styled_opt(generate_label, (!state.busy && !state.brief.trim().is_empty()).then(|| msg(AiPlanMsg::Generate)), button::primary),
        ]
        .spacing(8)
        .align_y(iced::Center)
        .into(),
        Source::Image => row![
            button(text("Choose image…").size(11))
                .on_press_maybe((!state.busy && !state.loading).then(|| msg(AiPlanMsg::PickImage)))
                .padding([4, 10]),
            text(match (&state.image, state.loading) {
                (_, true) => "Loading…".to_string(),
                (Some(i), _) => i.name.clone(),
                (None, _) => "A scanned or exported floor plan (PNG, JPEG…); a PDF can be exported to an image first.".into(),
            })
            .size(12)
            .width(Fill),
            text_input("optional notes, e.g. \"scale 1:100, walls 300\"", &state.brief)
                .on_input(|v| msg(AiPlanMsg::Brief(v)))
                .style(field_style)
                .size(12)
                .width(260),
            dialog_button_styled_opt(generate_label, (!state.busy && state.image.is_some()).then(|| msg(AiPlanMsg::Generate)), button::primary),
        ]
        .spacing(8)
        .align_y(iced::Center)
        .into(),
        Source::Selection => row![
            text("Selected lines and open polylines become wall centre lines; closed polylines become rooms.").size(12).width(Fill),
            labeled_field("Thickness".into(), &state.thickness, |v| msg(AiPlanMsg::Thickness(v)), 90.0),
            dialog_button_styled_opt("Use selection", (!state.busy).then(|| msg(AiPlanMsg::FromSelection)), button::primary),
        ]
        .spacing(8)
        .align_y(iced::Center)
        .into(),
    };

    let preview: Element<'_, Message> = match &state.plan {
        Some(plan) => canvas(Preview { plan, mm: state.mm_per_unit }).width(Fill).height(Fill).into(),
        None => container(text("The plan appears here for review before anything is drawn. Generate again with a change (\"make the kitchen 1 m wider\", \"add a window to bedroom 2\") to refine it.").size(12))
            .center(Fill)
            .into(),
    };
    let mut side = column![].spacing(6).width(260);
    if let Some(plan) = &state.plan {
        let doors = plan
            .openings
            .iter()
            .filter(|o| o.kind == OpeningKind::Door)
            .count();
        side = side.push(
            text(format!(
                "{} walls, {} doors, {} windows",
                plan.walls.len(),
                doors,
                plan.openings.len() - doors
            ))
            .size(12),
        );
        if !plan.notes.is_empty() {
            side = side.push(text(format!("Notes: {}", plan.notes)).size(11));
        }
        side = side.push(text("Rooms").size(12));
        for (n, r) in plan.rooms.iter().enumerate() {
            side = side.push(
                text_input("", &r.name)
                    .on_input(move |v| msg(AiPlanMsg::RoomName(n, v)))
                    .style(field_style)
                    .size(12),
            );
        }
        side = side.push(
            checkbox(state.schedule)
                .label("Room schedule")
                .on_toggle(|v| msg(AiPlanMsg::Schedule(v)))
                .size(14),
        );
        side = side.push(
            checkbox(state.build_3d)
                .label("3D massing model")
                .on_toggle(|v| msg(AiPlanMsg::Build3d(v)))
                .size(14),
        );
        if state.build_3d {
            side = side.push(labeled_field(
                "Wall height".into(),
                &state.wall_height,
                |v| msg(AiPlanMsg::WallHeight(v)),
                90.0,
            ));
        }
    }
    let body = row![
        container(preview)
            .style(container::bordered_box)
            .padding(4)
            .width(Fill)
            .height(Fill),
        scrollable(side).height(Fill),
    ]
    .spacing(10)
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
    let footer = row![
        text(state.estimate.as_str()).size(11).width(Fill),
        dialog_button_styled_opt(
            "Insert (one undo step)",
            (!state.busy && state.plan.is_some()).then(|| msg(AiPlanMsg::Insert)),
            button::primary
        ),
        dialog_button("Close", Message::CloseModal, false),
    ]
    .spacing(8)
    .align_y(iced::Center);

    container(column![header, hdivider(sizing.width), source, body, info, footer].spacing(10))
        .padding(16)
        .width(sizing.width)
        .height(sizing.height)
        .into()
}
