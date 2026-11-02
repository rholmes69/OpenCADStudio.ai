//! Image to model (AIIMAGE) and sketch to vector (AISKETCH): loads the
//! image, sends it to the vision model configured for the feature, and
//! inserts the reviewed result through the same plan applier as AICMD.

use super::{Message, ModalKind, OpenCADStudio};
use crate::ai::command_plan::Plan;
use crate::ai::image_to_model::{self as itm, Task as ImageTask};
use crate::ai::{AiFeature, ChatMessage, ChatRequest, ChatResponse, ImageData};
use crate::ui::window::ai_image::{AiImageMsg, Loaded, State};
use iced::Task;

fn feature(task: ImageTask) -> AiFeature {
    match task {
        ImageTask::MultiView => AiFeature::ImageToModel,
        ImageTask::Sketch => AiFeature::SketchToVector,
    }
}

/// Decode, downscale to [`itm::MAX_IMAGE_SIDE`] and re-encode as PNG for
/// sending; keep the original bytes for display.
#[cfg(not(target_arch = "wasm32"))]
pub(in crate::app) fn load_image(path: std::path::PathBuf) -> Result<Loaded, String> {
    let bytes = std::fs::read(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let decoded =
        image::load_from_memory(&bytes).map_err(|e| format!("not a readable image: {e}"))?;
    let (w, h) = (decoded.width(), decoded.height());
    let longest = w.max(h);
    let (send_bytes, send_size) = if longest > itm::MAX_IMAGE_SIDE {
        let small = decoded.resize(
            itm::MAX_IMAGE_SIDE,
            itm::MAX_IMAGE_SIDE,
            image::imageops::FilterType::Triangle,
        );
        let mut out = std::io::Cursor::new(Vec::new());
        small
            .write_to(&mut out, image::ImageFormat::Png)
            .map_err(|e| e.to_string())?;
        (out.into_inner(), (small.width(), small.height()))
    } else {
        (bytes.clone(), (w, h))
    };
    Ok(Loaded {
        name: path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        display: iced::widget::image::Handle::from_bytes(bytes),
        send: ImageData::from_bytes(&send_bytes),
        send_size,
    })
}

#[cfg(target_arch = "wasm32")]
pub(in crate::app) fn load_image(_path: std::path::PathBuf) -> Result<Loaded, String> {
    Err(crate::ai::client::DESKTOP_ONLY.into())
}

impl OpenCADStudio {
    /// The drawing's unit name when it is one the dialog offers.
    pub(in crate::app) fn ai_drawing_units(&self) -> Option<&'static str> {
        let code = self.tabs[self.active_tab]
            .scene
            .document
            .header
            .insertion_units as i64;
        match code {
            1 => Some("in"),
            2 => Some("ft"),
            4 => Some("mm"),
            5 => Some("cm"),
            6 => Some("m"),
            _ => None,
        }
    }

    pub(in crate::app) fn open_ai_image(&mut self, task: ImageTask) {
        if self.tabs[self.active_tab].is_start {
            self.command_line
                .push_error("Open a drawing or start a new one before using the AI tools.");
            return;
        }
        let units = self.ai_drawing_units().unwrap_or("mm").to_string();
        match self.ai_image.as_mut() {
            Some(state) if !state.busy => {
                if state.task != task {
                    state.task = task;
                    state.reading = None;
                    state.status = None;
                    state.build_3d = task == ImageTask::MultiView;
                    state.tidy = task == ImageTask::Sketch;
                }
            }
            Some(_) => {}
            None => self.ai_image = Some(State::new(task, units)),
        }
        self.active_modal = Some(ModalKind::AiImage);
        self.reset_modal_geometry();
        self.ai_image_estimate();
    }

    fn ai_image_request(&self, state: &State) -> Option<ChatRequest> {
        let image = state.image.as_ref()?;
        let system = itm::system_prompt(
            state.task,
            state.subject,
            &state.units,
            image.send_size.0,
            image.send_size.1,
        );
        let ask = match state.task {
            ImageTask::MultiView => "Read every view in this image.",
            ImageTask::Sketch => "Trace this drawing.",
        };
        Some(ChatRequest {
            messages: vec![
                ChatMessage::system(system),
                ChatMessage::user(ask).with_image(image.send.clone()),
            ],
            temperature: Some(0.0),
            json_mode: true,
        })
    }

    fn ai_image_estimate(&mut self) {
        let Some(state) = self.ai_image.as_ref() else {
            return;
        };
        let (settings, keys) = crate::ai::settings::current();
        let estimate = match settings.route(feature(state.task), &keys) {
            Ok(route) => {
                let mut line = match self.ai_image_request(state) {
                    Some(req) => crate::ai::describe_estimate(&route, &req),
                    None => format!("{} · {}", route.provider, route.model),
                };
                let info = crate::ai::model_info(&route);
                let known = !crate::ai::cached_models(route.provider).is_empty();
                if known && !info.vision {
                    line.push_str(
                        " · this model may not accept images — pick a vision model in AISETTINGS",
                    );
                }
                line
            }
            Err(e) => e,
        };
        if let Some(state) = self.ai_image.as_mut() {
            state.estimate = estimate;
        }
    }

    pub(in crate::app) fn on_ai_image(&mut self, msg: AiImageMsg) -> Task<Message> {
        let Some(state) = self.ai_image.as_mut() else {
            return Task::none();
        };
        match msg {
            AiImageMsg::Task(task) => {
                state.task = task;
                state.reading = None;
                state.status = None;
                state.warnings.clear();
                state.build_3d = task == ImageTask::MultiView;
                state.tidy = task == ImageTask::Sketch;
                state.confirm_armed = false;
            }
            AiImageMsg::Pick => {
                return Task::perform(crate::io::pick_image_file(), |r| {
                    Message::AiImage(AiImageMsg::Picked(r.map(|(path, _, _)| path)))
                });
            }
            AiImageMsg::Picked(Err(_)) => {}
            AiImageMsg::Picked(Ok(path)) => {
                state.loading = true;
                state.status = None;
                return Task::perform(crate::ai::run_blocking(move || load_image(path)), |r| {
                    Message::AiImage(AiImageMsg::Loaded(r))
                });
            }
            AiImageMsg::Loaded(result) => {
                state.loading = false;
                match result {
                    Ok(image) => {
                        state.image = Some(image);
                        state.reading = None;
                        state.warnings.clear();
                        state.confirm_armed = false;
                    }
                    Err(e) => state.status = Some(Err(e)),
                }
            }
            AiImageMsg::Subject(s) => {
                state.subject = s;
                state.confirm_armed = false;
            }
            AiImageMsg::Units(u) => {
                state.units = u;
                state.confirm_armed = false;
            }
            AiImageMsg::Build3d(v) => state.build_3d = v,
            AiImageMsg::Tidy(v) => state.tidy = v,
            AiImageMsg::Analyze => return self.ai_image_analyze(),
            AiImageMsg::Reply(result) => return self.ai_image_reply(result),
            AiImageMsg::Insert => return self.ai_image_insert(),
            AiImageMsg::OpenSettings => {
                self.open_ai_settings();
                return Task::none();
            }
        }
        self.ai_image_estimate();
        Task::none()
    }

    fn ai_image_analyze(&mut self) -> Task<Message> {
        let (settings, keys) = crate::ai::settings::current();
        let Some(state) = self.ai_image.as_ref() else {
            return Task::none();
        };
        if state.busy || state.image.is_none() {
            return Task::none();
        }
        let route = match settings.route(feature(state.task), &keys) {
            Ok(r) => r,
            Err(e) => {
                if let Some(state) = self.ai_image.as_mut() {
                    state.status =
                        Some(Err(format!("{e}. Open Settings to choose a vision model.")));
                }
                return Task::none();
            }
        };
        if settings.confirm_before_send && !route.is_local && !state.confirm_armed {
            self.ai_image_estimate();
            if let Some(state) = self.ai_image.as_mut() {
                state.confirm_armed = true;
            }
            return Task::none();
        }
        let Some(request) = self.ai_image_request(state) else {
            return Task::none();
        };
        if let Some(state) = self.ai_image.as_mut() {
            state.confirm_armed = false;
            state.busy = true;
            state.retries = 0;
            state.status = None;
        }
        Self::ai_image_send(route, request)
    }

    fn ai_image_send(route: crate::ai::ResolvedRoute, request: ChatRequest) -> Task<Message> {
        Task::perform(
            crate::ai::run_blocking(move || crate::ai::client::chat(&route, &request)),
            |r| Message::AiImage(AiImageMsg::Reply(r)),
        )
    }

    fn ai_image_reply(&mut self, result: Result<ChatResponse, String>) -> Task<Message> {
        let (settings, keys) = crate::ai::settings::current();
        let Some(state) = self.ai_image.as_mut() else {
            return Task::none();
        };
        state.busy = false;
        let response = match result {
            Ok(r) => r,
            Err(e) => {
                state.status = Some(Err(e));
                return Task::none();
            }
        };
        match itm::parse_reading(&response.content) {
            Ok(mut reading) => {
                if reading.units.is_empty() || itm::mm_per_unit(&reading.units).is_none() {
                    reading.units = state.units.clone();
                }
                if state.task == ImageTask::Sketch && state.tidy {
                    for view in reading.views.iter_mut() {
                        let weld = 0.005 * view.width.max(view.height);
                        itm::tidy_sketch(view, 3.0, weld);
                    }
                }
                state.warnings = itm::consistency_warnings(&reading);
                let shapes: usize = reading.views.iter().map(|v| v.shapes.len()).sum();
                let usage = response
                    .usage
                    .map(|u| {
                        format!(
                            " ({} in / {} out tokens)",
                            u.prompt_tokens, u.completion_tokens
                        )
                    })
                    .unwrap_or_default();
                state.status = Some(Ok(format!(
                    "Found {} view(s) and {shapes} shape(s){usage}. Check the overlay, then Insert.",
                    reading.views.len()
                )));
                state.reading = Some(reading);
                Task::none()
            }
            Err(e) if state.retries == 0 => {
                // One automatic retry with the reason, like AICMD.
                state.retries = 1;
                state.status = Some(Err(format!("{e} — asking the model again…")));
                let Ok(route) = settings.route(feature(state.task), &keys) else {
                    return Task::none();
                };
                let Some(mut request) = self.ai_image_request(self.ai_image.as_ref().unwrap())
                else {
                    return Task::none();
                };
                request
                    .messages
                    .push(ChatMessage::assistant(response.content));
                request.messages.push(ChatMessage::user(format!(
                    "That answer could not be used: {e}. Reply again with only the JSON object described."
                )));
                if let Some(state) = self.ai_image.as_mut() {
                    state.busy = true;
                }
                Self::ai_image_send(route, request)
            }
            Err(e) => {
                state.status = Some(Err(format!("The model's answer could not be used: {e}")));
                Task::none()
            }
        }
    }

    /// Steps that insert `reading` into the drawing (2D views, then the
    /// optional 3D solid), placed to the right of what is already drawn.
    pub(in crate::app) fn ai_image_plan(
        &self,
        mut reading: itm::Reading,
        build_3d: bool,
    ) -> Result<Plan, String> {
        // Convert the reading into the drawing's own units.
        if let (Some(from), Some(to)) = (
            itm::mm_per_unit(&reading.units),
            self.ai_drawing_units().and_then(itm::mm_per_unit),
        ) {
            itm::scale_reading(&mut reading, from / to);
        }
        let extent = |r: &itm::Reading| {
            let offsets = itm::layout(r, [0.0, 0.0]);
            r.views
                .iter()
                .zip(&offsets)
                .map(|(v, o)| o[0] + v.width)
                .fold(0.0_f64, f64::max)
        };
        let size = reading
            .views
            .iter()
            .map(|v| v.width.max(v.height))
            .fold(0.0_f64, f64::max);
        let gap = size * 0.25;
        let origin = match self.tabs[self.active_tab].scene.model_space_extents() {
            Some((min, max)) if max.x > min.x => [max.x as f64 + gap, min.y as f64],
            _ => [0.0, 0.0],
        };
        let mut steps = itm::plan_2d(&reading, origin);
        if build_3d {
            let x = origin[0] + extent(&reading) + gap;
            steps.extend(itm::plan_3d(&reading, [x, origin[1]])?);
        }
        Ok(Plan {
            summary: format!("{} view(s) from an image", reading.views.len()),
            message: None,
            steps,
        })
    }

    fn ai_image_insert(&mut self) -> Task<Message> {
        let Some(state) = self.ai_image.as_ref() else {
            return Task::none();
        };
        let Some(reading) = state.reading.clone() else {
            return Task::none();
        };
        let build_3d = state.task == ImageTask::MultiView && state.build_3d;
        let plan = match self.ai_image_plan(reading.clone(), build_3d) {
            Ok(p) => p,
            Err(e) => {
                if let Some(state) = self.ai_image.as_mut() {
                    state.status = Some(Err(format!(
                        "{e}. Untick the 3D option to insert only the views."
                    )));
                }
                return Task::none();
            }
        };
        match self.apply_ai_plan(&plan) {
            Ok(_) => {
                let what = if build_3d {
                    "views and a 3D solid"
                } else {
                    "geometry"
                };
                self.command_line.push_info(&format!(
                    "AIIMAGE: inserted {what} from {} view(s) on AI-* layers; one UNDO removes it.",
                    reading.views.len()
                ));
                self.ai_image = None;
                self.active_modal = None;
            }
            Err(e) => {
                if let Some(state) = self.ai_image.as_mut() {
                    state.status = Some(Err(e));
                }
            }
        }
        Task::none()
    }
}

#[cfg(test)]
mod tests {
    use super::OpenCADStudio;
    use crate::ai::image_to_model::parse_reading;

    const SHEET: &str = r#"{"units":"mm","views":[
        {"name":"FRONT","kind":"front","bbox_px":[100,500,500,700],"width":80,"height":40,
         "outline":[[0,0],[80,0],[80,40],[40,40],[40,20],[0,20]],
         "entities":[{"type":"polyline","pts":[[0,0],[80,0],[80,40],[40,40],[40,20],[0,20]],"closed":true}]},
        {"name":"TOP","kind":"top","bbox_px":[100,200,500,450],"width":80,"height":50,
         "outline":[[0,0],[80,0],[80,50],[0,50]],"entities":[{"type":"polyline","pts":[[0,0],[80,0],[80,50],[0,50]],"closed":true}]},
        {"name":"RIGHT","kind":"right","bbox_px":[550,500,800,700],"width":50,"height":40,
         "outline":[[0,0],[50,0],[50,40],[0,40]],"entities":[{"type":"polyline","pts":[[0,0],[50,0],[50,40],[0,40]],"closed":true}]}
    ]}"#;

    #[test]
    fn inserts_views_and_an_intersected_solid_with_the_right_extents() {
        let mut app = OpenCADStudio::new_for_test();
        app.automation_op(r#"{"op":"new"}"#);
        let i = app.active_tab;
        app.tabs[i].scene.document.header.insertion_units = 4; // millimetres
        let reading = parse_reading(SHEET).unwrap();
        let plan = app.ai_image_plan(reading, true).expect("plan");
        let report = app.apply_ai_plan(&plan).expect("applies");
        assert!(report.contains("step"), "{report}");
        let solids = app.automation_op(r#"{"op":"query","type":"3D Solid","detail":"summary"}"#);
        assert_eq!(solids["count"], 1, "{solids}");
        let handle = solids["entities"][0]["handle"]
            .as_str()
            .unwrap()
            .to_string();
        let measured = app.automation_op(
            &serde_json::json!({"protocol":1,"op":"measure","handles":[handle]}).to_string(),
        );
        let r = &measured["measurements"][0];
        let (min, max) = (&r["bounds"]["min"], &r["bounds"]["max"]);
        let size = |i: usize| max[i].as_f64().unwrap() - min[i].as_f64().unwrap();
        assert!((size(0) - 80.0).abs() < 0.01, "width {}", size(0));
        assert!((size(1) - 50.0).abs() < 0.01, "depth {}", size(1));
        assert!((size(2) - 40.0).abs() < 0.01, "height {}", size(2));
        // The stepped front profile cuts the solid: less than the full box.
        let volume = r["mesh"]["volume"].as_f64().expect("volume");
        assert!(
            (volume - (80.0 * 50.0 * 40.0 - 40.0 * 50.0 * 20.0)).abs() < 1.0,
            "volume {volume}"
        );
        // The three 2D views are there too, on their layers.
        let front = app.automation_op(r#"{"op":"query","layer":"AI-FRONT"}"#);
        assert_eq!(front["count"], 1);
        // One undo removes everything.
        app.automation_op(r#"{"op":"undo"}"#);
        let all = app.automation_op(r#"{"op":"entities"}"#);
        assert_eq!(all["total"], 0, "{all}");
    }

    /// Front and top are full boxes; the side view is an L that is full
    /// height near the front (Y < 25) and half height behind. A left view
    /// shows the same part mirrored, so both must give the same solid.
    fn stepped_side(kind: &str, outline: &str) -> String {
        format!(
            r#"{{"units":"mm","views":[
            {{"kind":"front","bbox_px":[0,300,400,500],"width":80,"height":40,"outline":[[0,0],[80,0],[80,40],[0,40]],"entities":[]}},
            {{"kind":"top","bbox_px":[0,0,400,250],"width":80,"height":50,"outline":[[0,0],[80,0],[80,50],[0,50]],"entities":[]}},
            {{"kind":"{kind}","bbox_px":[450,300,700,500],"width":50,"height":40,"outline":{outline},"entities":[]}}]}}"#
        )
    }

    fn solid_centroid_y(sheet: &str) -> (f64, f64) {
        let mut app = OpenCADStudio::new_for_test();
        app.automation_op(r#"{"op":"new"}"#);
        let i = app.active_tab;
        app.tabs[i].scene.document.header.insertion_units = 4;
        let plan = app
            .ai_image_plan(parse_reading(sheet).unwrap(), true)
            .unwrap();
        app.apply_ai_plan(&plan).expect("applies");
        let solids = app.automation_op(r#"{"op":"query","type":"3D Solid"}"#);
        let handle = solids["entities"][0]["handle"]
            .as_str()
            .unwrap()
            .to_string();
        let m = app.automation_op(
            &serde_json::json!({"protocol":1,"op":"measure","handles":[handle]}).to_string(),
        );
        let mesh = &m["measurements"][0]["mesh"];
        (
            mesh["volume"].as_f64().unwrap(),
            mesh["centroid"][1].as_f64().unwrap(),
        )
    }

    #[test]
    fn side_views_map_to_depth_and_left_views_are_mirrored() {
        let (v_right, y_right) = solid_centroid_y(&stepped_side(
            "right",
            "[[0,0],[50,0],[50,20],[25,20],[25,40],[0,40]]",
        ));
        let (v_left, y_left) = solid_centroid_y(&stepped_side(
            "left",
            "[[0,0],[50,0],[50,40],[25,40],[25,20],[0,20]]",
        ));
        let expected = (50.0 * 20.0 + 25.0 * 20.0) * 80.0;
        assert!((v_right - expected).abs() < 1.0, "{v_right}");
        assert!((v_left - expected).abs() < 1.0, "{v_left}");
        assert!(
            y_right < 25.0,
            "mass sits at the front: centroid y {y_right}"
        );
        assert!((y_right - y_left).abs() < 1e-6, "{y_right} vs {y_left}");
    }
}
