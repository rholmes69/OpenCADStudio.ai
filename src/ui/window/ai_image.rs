//! Image to model (AIIMAGE) and sketch to vector (AISKETCH) dialog: pick an
//! image, let the vision model read it, review the detected geometry drawn
//! over the image, then insert it.

use crate::ai::image_to_model::{view_label, Reading, Shape, Subject, Task};
use crate::ai::{ChatResponse, ImageData};
use crate::app::Message;
use crate::ui::style::form::{
    dialog_button, dialog_button_styled_opt, hdivider, labeled_pick_list,
};
use iced::mouse;
use iced::widget::{button, canvas, checkbox, column, container, row, scrollable, text, Space};
use iced::{Color, Element, Fill, Point, Rectangle, Size, Theme};

/// The picked image: shown full size, sent downscaled.
#[derive(Debug, Clone)]
pub struct Loaded {
    pub name: String,
    pub display: iced::widget::image::Handle,
    pub send: ImageData,
    /// Size of the image the model sees (its pixel coordinates).
    pub send_size: (u32, u32),
}

#[derive(Debug, Clone)]
pub enum AiImageMsg {
    Task(Task),
    Pick,
    Picked(Result<std::path::PathBuf, String>),
    Loaded(Result<Loaded, String>),
    Subject(Subject),
    Units(String),
    Build3d(bool),
    Tidy(bool),
    Analyze,
    Reply(Result<ChatResponse, String>),
    Insert,
    OpenSettings,
}

pub const UNITS: [&str; 5] = ["mm", "cm", "m", "in", "ft"];

pub(crate) struct State {
    pub task: Task,
    pub subject: Subject,
    pub units: String,
    pub build_3d: bool,
    pub tidy: bool,
    pub image: Option<Loaded>,
    pub loading: bool,
    pub busy: bool,
    pub reading: Option<Reading>,
    pub warnings: Vec<String>,
    pub status: Option<Result<String, String>>,
    pub estimate: String,
    pub confirm_armed: bool,
    pub retries: u8,
}

impl State {
    pub fn new(task: Task, units: String) -> Self {
        Self {
            task,
            subject: Subject::Part,
            units,
            build_3d: task == Task::MultiView,
            tidy: task == Task::Sketch,
            image: None,
            loading: false,
            busy: false,
            reading: None,
            warnings: Vec::new(),
            status: None,
            estimate: String::new(),
            confirm_armed: false,
            retries: 0,
        }
    }
}

fn msg(m: AiImageMsg) -> Message {
    Message::AiImage(m)
}

/// The image with the detected views and shapes drawn over it.
struct Overlay<'a> {
    image: &'a Loaded,
    reading: Option<&'a Reading>,
}

impl canvas::Program<Message> for Overlay<'_> {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &iced::Renderer,
        theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        let (iw, ih) = (
            self.image.send_size.0.max(1) as f32,
            self.image.send_size.1.max(1) as f32,
        );
        let fit = (bounds.width / iw).min(bounds.height / ih);
        let (dw, dh) = (iw * fit, ih * fit);
        let (ox, oy) = ((bounds.width - dw) / 2.0, (bounds.height - dh) / 2.0);
        frame.draw_image(
            Rectangle::new(Point::new(ox, oy), Size::new(dw, dh)),
            &self.image.display,
        );
        let Some(reading) = self.reading else {
            return vec![frame.into_geometry()];
        };
        let palette = theme.palette();
        let visible = Color::from_rgb(0.95, 0.15, 0.55);
        let hidden = Color::from_rgba(0.1, 0.55, 0.95, 0.9);
        let frame_color = palette.primary.strong.color;
        for view in &reading.views {
            let [x0, y0, x1, y1] = view.bbox_px.map(|v| v as f32);
            let (sx, sy) = (
                (x1 - x0) / view.width as f32,
                (y1 - y0) / view.height as f32,
            );
            // View-local drawing units → canvas pixels.
            let map = |p: [f64; 2]| {
                Point::new(
                    ox + (x0 + p[0] as f32 * sx) * fit,
                    oy + (y1 - p[1] as f32 * sy) * fit,
                )
            };
            frame.stroke(
                &canvas::Path::rectangle(
                    Point::new(ox + x0 * fit, oy + y0 * fit),
                    Size::new((x1 - x0) * fit, (y1 - y0) * fit),
                ),
                canvas::Stroke::default()
                    .with_color(frame_color)
                    .with_width(1.0),
            );
            frame.fill_text(canvas::Text {
                content: view_label(view),
                position: Point::new(ox + x0 * fit, oy + y0 * fit - 14.0),
                color: frame_color,
                size: 12.0.into(),
                ..Default::default()
            });
            let polyline = |pts: &[[f64; 2]], closed: bool| {
                canvas::Path::new(|b| {
                    if let Some(first) = pts.first() {
                        b.move_to(map(*first));
                        for p in &pts[1..] {
                            b.line_to(map(*p));
                        }
                        if closed {
                            b.close();
                        }
                    }
                })
            };
            let arc_pts = |c: [f64; 2], r: f64, a0: f64, a1: f64| -> Vec<[f64; 2]> {
                let mut sweep = a1 - a0;
                while sweep <= 0.0 {
                    sweep += 360.0;
                }
                let n = ((sweep / 360.0) * 48.0).ceil().max(4.0) as usize;
                (0..=n)
                    .map(|k| {
                        let t = (a0 + sweep * k as f64 / n as f64).to_radians();
                        [c[0] + r * t.cos(), c[1] + r * t.sin()]
                    })
                    .collect()
            };
            for s in &view.shapes {
                let stroke = canvas::Stroke::default()
                    .with_color(if s.hidden() { hidden } else { visible })
                    .with_width(1.5);
                let path = match s {
                    Shape::Line { a, b, .. } => polyline(&[*a, *b], false),
                    Shape::Circle { c, r, .. } => polyline(&arc_pts(*c, *r, 0.0, 360.0), true),
                    Shape::Arc {
                        c, r, start, end, ..
                    } => polyline(&arc_pts(*c, *r, *start, *end), false),
                    Shape::Polyline { pts, closed, .. } => polyline(pts, *closed),
                    Shape::Text { at, value, .. } => {
                        frame.fill_text(canvas::Text {
                            content: value.clone(),
                            position: map(*at),
                            color: visible,
                            size: 11.0.into(),
                            ..Default::default()
                        });
                        continue;
                    }
                };
                frame.stroke(&path, stroke);
            }
        }
        vec![frame.into_geometry()]
    }
}

pub(crate) fn view(state: &State, sizing: crate::ui::modal::ModalSizing) -> Element<'_, Message> {
    let task_button = |task: Task, label: &'static str| {
        let active = state.task == task;
        button(text(label).size(11))
            .on_press_maybe((!active && !state.busy).then(|| msg(AiImageMsg::Task(task))))
            .style(if active {
                button::primary
            } else {
                button::secondary
            })
            .padding([4, 10])
    };
    let header = row![
        task_button(Task::MultiView, "Views to model (AIIMAGE)"),
        task_button(Task::Sketch, "Sketch to vector (AISKETCH)"),
        Space::new().width(Fill),
        button(text("Settings…").size(11))
            .on_press(msg(AiImageMsg::OpenSettings))
            .padding([4, 10]),
    ]
    .spacing(6)
    .align_y(iced::Center);

    let pick_label = match (&state.image, state.loading) {
        (_, true) => "Loading…".to_string(),
        (Some(img), _) => format!(
            "{}  ({}×{} sent)",
            img.name, img.send_size.0, img.send_size.1
        ),
        (None, _) => "No image chosen".to_string(),
    };
    let options = column![
        row![
            button(text("Choose image…").size(11))
                .on_press_maybe((!state.busy && !state.loading).then(|| msg(AiImageMsg::Pick)))
                .padding([4, 10]),
            text(pick_label).size(12),
        ]
        .spacing(8)
        .align_y(iced::Center),
        row![
            labeled_pick_list(
                "Subject".into(),
                Subject::ALL.to_vec(),
                Some(state.subject),
                |s| msg(AiImageMsg::Subject(s)),
                iced::Length::Fixed(170.0)
            ),
            labeled_pick_list(
                "Units".into(),
                UNITS.iter().map(|u| u.to_string()).collect(),
                Some(state.units.clone()),
                |u| msg(AiImageMsg::Units(u)),
                iced::Length::Fixed(80.0)
            ),
        ]
        .spacing(16),
        match state.task {
            Task::MultiView => checkbox(state.build_3d)
                .label("Also build a 3D solid from the front, top and side silhouettes")
                .on_toggle(|v| msg(AiImageMsg::Build3d(v)))
                .size(14),
            Task::Sketch => checkbox(state.tidy)
                .label("Straighten near-horizontal/vertical lines and join nearly-touching ends")
                .on_toggle(|v| msg(AiImageMsg::Tidy(v)))
                .size(14),
        },
    ]
    .spacing(8);

    let preview: Element<'_, Message> = match &state.image {
        Some(image) => canvas(Overlay {
            image,
            reading: state.reading.as_ref(),
        })
        .width(Fill)
        .height(Fill)
        .into(),
        None => container(text(match state.task {
            Task::MultiView => "Choose an image showing 2 to 9 views of one part or building (front, top/plan, sides, iso). Dimensioned drawings give the most accurate result.",
            Task::Sketch => "Choose a photo of a hand sketch or a scanned drawing. A straight, evenly lit, cropped photo works best.",
        }).size(12))
        .center(Fill)
        .into(),
    };
    let preview = container(preview)
        .style(container::bordered_box)
        .padding(4)
        .width(Fill)
        .height(Fill);

    let mut info = column![].spacing(4);
    if let Some(reading) = &state.reading {
        let views = reading
            .views
            .iter()
            .map(|v| {
                format!(
                    "{}: {} × {} {}, {} shape(s)",
                    view_label(v),
                    trim(v.width),
                    trim(v.height),
                    reading.units,
                    v.shapes.len()
                )
            })
            .collect::<Vec<_>>()
            .join("   ·   ");
        info = info.push(text(views).size(12));
        if !reading.notes.is_empty() {
            info = info.push(text(format!("Notes: {}", reading.notes)).size(11));
        }
        for w in &state.warnings {
            info = info.push(text(format!("Check: {w}")).size(11).style(text::warning));
        }
    }
    match &state.status {
        Some(Ok(s)) => info = info.push(text(s.as_str()).size(12).style(text::success)),
        Some(Err(e)) => info = info.push(text(e.as_str()).size(12).style(text::danger)),
        None => {}
    }
    if state.busy {
        info = info.push(text("Reading the image… (vision models can take a minute)").size(12));
    }
    let info = scrollable(info).height(iced::Length::Shrink);

    let analyze_label = if state.confirm_armed {
        "Confirm & analyze"
    } else if state.reading.is_some() {
        "Analyze again"
    } else {
        "Analyze"
    };
    let footer = row![
        text(state.estimate.as_str()).size(11).width(Fill),
        dialog_button_styled_opt(
            analyze_label,
            (!state.busy && state.image.is_some()).then(|| msg(AiImageMsg::Analyze)),
            if state.reading.is_some() {
                button::secondary
            } else {
                button::primary
            }
        ),
        dialog_button_styled_opt(
            "Insert (one undo step)",
            (!state.busy && state.reading.is_some()).then(|| msg(AiImageMsg::Insert)),
            button::primary
        ),
        dialog_button("Close", Message::CloseModal, false),
    ]
    .spacing(8)
    .align_y(iced::Center);

    container(
        column![
            header,
            hdivider(sizing.width),
            options,
            preview,
            info,
            footer
        ]
        .spacing(10),
    )
    .padding(16)
    .width(sizing.width)
    .height(sizing.height)
    .into()
}

fn trim(x: f64) -> String {
    let s = format!("{x:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}
