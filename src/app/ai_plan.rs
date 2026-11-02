//! Floor-plan assistant (AIPLAN): plans from a brief or a plan image (via
//! the model set for *Floor plan assistant*) or from selected lines, drawn
//! through the shared plan applier as one undo step.

use super::{Message, ModalKind, OpenCADStudio};
use crate::ai::command_plan::Plan as StepPlan;
use crate::ai::floor_plan::{self as fp, Plan};
use crate::ai::image_to_model::mm_per_unit;
use crate::ai::{AiFeature, ChatMessage, ChatRequest, ChatResponse};
use crate::ui::window::ai_plan::{AiPlanMsg, Source, State};
use codec::EntityType;
use iced::Task;

fn fmt(x: f64) -> String {
    let s = format!("{x:.4}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

impl OpenCADStudio {
    fn ai_plan_mm(&self) -> Option<f64> {
        self.ai_drawing_units().and_then(mm_per_unit)
    }

    pub(in crate::app) fn open_ai_plan(&mut self) {
        if self.tabs[self.active_tab].is_start {
            self.command_line
                .push_error("Open a drawing or start a new one before using AIPLAN.");
            return;
        }
        if self.ai_plan.is_none() {
            // Defaults in drawing units: 200 mm walls, 2.7 m storey.
            let mm = self.ai_plan_mm().unwrap_or(1000.0);
            self.ai_plan = Some(State {
                source: Source::Brief,
                brief: String::new(),
                image: None,
                loading: false,
                thickness: fmt(200.0 / mm),
                plan: None,
                schedule: true,
                build_3d: false,
                wall_height: fmt(2700.0 / mm),
                mm_per_unit: None,
                busy: false,
                status: None,
                estimate: String::new(),
                confirm_armed: false,
                retries: 0,
            });
        }
        self.active_modal = Some(ModalKind::AiPlan);
        self.reset_modal_geometry();
        self.ai_plan_estimate();
    }

    fn ai_plan_request(&self) -> Option<ChatRequest> {
        let state = self.ai_plan.as_ref()?;
        let mut messages = Vec::new();
        match state.source {
            Source::Brief => {
                messages.push(ChatMessage::system(fp::brief_prompt("m")));
                if let Some(plan) = &state.plan {
                    messages.push(ChatMessage::assistant(fp::to_json(plan).to_string()));
                }
                messages.push(ChatMessage::user(state.brief.trim()));
            }
            Source::Image => {
                let image = state.image.as_ref()?;
                messages.push(ChatMessage::system(fp::image_prompt(
                    "m",
                    image.send_size.0,
                    image.send_size.1,
                )));
                let note = if state.brief.trim().is_empty() {
                    "Convert this floor plan.".to_string()
                } else {
                    format!("Convert this floor plan. Notes: {}", state.brief.trim())
                };
                messages.push(ChatMessage::user(note).with_image(image.send.clone()));
            }
            Source::Selection => return None,
        }
        Some(ChatRequest {
            messages,
            temperature: Some(0.1),
            json_mode: true,
        })
    }

    fn ai_plan_estimate(&mut self) {
        let (settings, keys) = crate::ai::settings::current();
        let estimate = match (
            self.ai_plan.as_ref().map(|s| s.source),
            settings.route(AiFeature::FloorPlan, &keys),
        ) {
            (Some(Source::Selection), _) => {
                "Plans from selected lines are built locally — no model is used.".into()
            }
            (_, Ok(route)) => self
                .ai_plan_request()
                .map(|r| crate::ai::describe_estimate(&route, &r))
                .unwrap_or_else(|| format!("{} · {}", route.provider, route.model)),
            (_, Err(e)) => e,
        };
        if let Some(state) = self.ai_plan.as_mut() {
            state.estimate = estimate;
        }
    }

    /// Walls and rooms from the selected lines and polylines.
    fn ai_plan_from_selection(&mut self) -> Result<Plan, String> {
        let i = self.active_tab;
        let thickness: f64 = self
            .ai_plan
            .as_ref()
            .and_then(|s| s.thickness.trim().parse().ok())
            .filter(|t: &f64| *t > 0.0)
            .ok_or("enter a positive wall thickness")?;
        let doc = &self.tabs[i].scene.document;
        let mut segments = Vec::new();
        let mut polygons = Vec::new();
        for h in self.tabs[i].scene.selected_handles_in_order() {
            match doc.get_entity(h) {
                Some(EntityType::Line(l)) => {
                    segments.push(([l.start.x, l.start.y], [l.end.x, l.end.y]))
                }
                Some(EntityType::LwPolyline(p)) => {
                    let Some(world) = crate::entities::curve::lwpolyline_world_xy(p) else {
                        continue;
                    };
                    let pts: Vec<[f64; 2]> = world
                        .vertices
                        .iter()
                        .map(|v| [v.location.x, v.location.y])
                        .collect();
                    if world.is_closed && pts.len() >= 3 {
                        polygons.push(pts);
                    } else {
                        segments.extend(pts.windows(2).map(|w| (w[0], w[1])));
                    }
                }
                _ => {}
            }
        }
        if segments.is_empty() {
            return Err("select lines or open polylines to use as wall centre lines".into());
        }
        let plan = fp::from_geometry(
            &segments,
            &polygons,
            thickness,
            self.ai_drawing_units().unwrap_or(""),
        );
        fp::check(&plan)?;
        Ok(plan)
    }

    pub(in crate::app) fn on_ai_plan(&mut self, msg: AiPlanMsg) -> Task<Message> {
        let mm_drawing = self.ai_plan_mm();
        let Some(state) = self.ai_plan.as_mut() else {
            return Task::none();
        };
        match msg {
            AiPlanMsg::Source(s) => {
                state.source = s;
                state.confirm_armed = false;
            }
            AiPlanMsg::Brief(v) => {
                state.brief = v;
                state.confirm_armed = false;
            }
            AiPlanMsg::Thickness(v) => state.thickness = v,
            AiPlanMsg::PickImage => {
                return Task::perform(crate::io::pick_image_file(), |r| {
                    Message::AiPlan(AiPlanMsg::ImagePicked(r.map(|(p, _, _)| p)))
                });
            }
            AiPlanMsg::ImagePicked(Err(_)) => {}
            AiPlanMsg::ImagePicked(Ok(path)) => {
                state.loading = true;
                return Task::perform(
                    crate::ai::run_blocking(move || super::ai_image::load_image(path)),
                    |r| Message::AiPlan(AiPlanMsg::ImageLoaded(r)),
                );
            }
            AiPlanMsg::ImageLoaded(r) => {
                state.loading = false;
                match r {
                    Ok(img) => {
                        state.image = Some(img);
                        state.confirm_armed = false;
                    }
                    Err(e) => state.status = Some(Err(e)),
                }
            }
            AiPlanMsg::FromSelection => match self.ai_plan_from_selection() {
                Ok(plan) => {
                    if let Some(state) = self.ai_plan.as_mut() {
                        state.status = Some(Ok(format!(
                                "{} wall(s) and {} room(s) from the selection. Rename the rooms, then Insert.",
                                plan.walls.len(),
                                plan.rooms.len()
                            )));
                        state.mm_per_unit = mm_drawing;
                        state.plan = Some(plan);
                    }
                }
                Err(e) => {
                    if let Some(state) = self.ai_plan.as_mut() {
                        state.status = Some(Err(e));
                    }
                }
            },
            AiPlanMsg::Generate => return self.ai_plan_generate(),
            AiPlanMsg::Reply(r) => return self.ai_plan_reply(r),
            AiPlanMsg::RoomName(n, v) => {
                if let Some(room) = state.plan.as_mut().and_then(|p| p.rooms.get_mut(n)) {
                    room.name = v;
                }
                return Task::none();
            }
            AiPlanMsg::Schedule(v) => state.schedule = v,
            AiPlanMsg::Build3d(v) => state.build_3d = v,
            AiPlanMsg::WallHeight(v) => state.wall_height = v,
            AiPlanMsg::Insert => {
                match self.ai_plan_insert() {
                    Ok(report) => {
                        self.command_line.push_info(&format!("AIPLAN: {report}"));
                        self.ai_plan = None;
                        self.active_modal = None;
                    }
                    Err(e) => {
                        if let Some(state) = self.ai_plan.as_mut() {
                            state.status = Some(Err(e));
                        }
                    }
                }
                return Task::none();
            }
            AiPlanMsg::OpenSettings => {
                self.open_ai_settings();
                return Task::none();
            }
        }
        self.ai_plan_estimate();
        Task::none()
    }

    fn ai_plan_generate(&mut self) -> Task<Message> {
        let (settings, keys) = crate::ai::settings::current();
        let route = match settings.route(AiFeature::FloorPlan, &keys) {
            Ok(r) => r,
            Err(e) => {
                if let Some(state) = self.ai_plan.as_mut() {
                    state.status = Some(Err(format!("{e}. Open Settings to choose a model.")));
                }
                return Task::none();
            }
        };
        let armed = self.ai_plan.as_ref().is_some_and(|s| s.confirm_armed);
        if settings.confirm_before_send && !route.is_local && !armed {
            self.ai_plan_estimate();
            if let Some(state) = self.ai_plan.as_mut() {
                state.confirm_armed = true;
            }
            return Task::none();
        }
        let Some(request) = self.ai_plan_request() else {
            return Task::none();
        };
        if let Some(state) = self.ai_plan.as_mut() {
            state.busy = true;
            state.confirm_armed = false;
            state.retries = 0;
            state.status = None;
        }
        Self::ai_plan_send(route, request)
    }

    fn ai_plan_send(route: crate::ai::ResolvedRoute, request: ChatRequest) -> Task<Message> {
        Task::perform(
            crate::ai::run_blocking(move || crate::ai::client::chat(&route, &request)),
            |r| Message::AiPlan(AiPlanMsg::Reply(r)),
        )
    }

    fn ai_plan_reply(&mut self, result: Result<ChatResponse, String>) -> Task<Message> {
        let (settings, keys) = crate::ai::settings::current();
        let request = self.ai_plan_request();
        let drawing_mm = self.ai_plan_mm().unwrap_or(1000.0);
        let Some(state) = self.ai_plan.as_mut() else {
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
        match fp::parse_plan(&response.content) {
            Ok(plan) => {
                let units = if plan.units.is_empty() {
                    "m"
                } else {
                    plan.units.as_str()
                };
                state.mm_per_unit = mm_per_unit(units);
                let total: f64 = plan
                    .rooms
                    .iter()
                    .map(|r| fp::area_perimeter(&r.points).0)
                    .sum();
                state.status = Some(Ok(format!(
                    "{} room(s), {} in total. Check the preview, rename rooms if needed, then Insert.",
                    plan.rooms.len(),
                    fp::area_label(total, state.mm_per_unit)
                )));
                // The plan's storey height, in drawing units, for the massing.
                if plan.wall_height > 0.0 {
                    state.wall_height =
                        fmt(plan.wall_height * state.mm_per_unit.unwrap_or(1000.0) / drawing_mm);
                }
                state.plan = Some(plan);
                Task::none()
            }
            Err(e) if state.retries == 0 => {
                state.retries = 1;
                state.status = Some(Err(format!("{e} — asking the model again…")));
                let (Ok(route), Some(mut request)) =
                    (settings.route(AiFeature::FloorPlan, &keys), request)
                else {
                    return Task::none();
                };
                request
                    .messages
                    .push(ChatMessage::assistant(response.content));
                request.messages.push(ChatMessage::user(format!(
                    "That plan could not be used: {e}. Reply again with only the corrected JSON."
                )));
                state.busy = true;
                Self::ai_plan_send(route, request)
            }
            Err(e) => {
                state.status = Some(Err(e));
                Task::none()
            }
        }
    }

    /// Draw the reviewed plan (and schedule / massing) as one undo step.
    pub(in crate::app) fn ai_plan_insert(&mut self) -> Result<String, String> {
        let i = self.active_tab;
        let state = self.ai_plan.as_ref().ok_or("nothing to insert")?;
        let mut plan = state
            .plan
            .clone()
            .ok_or("generate or select a plan first")?;
        let drawing_mm = self.ai_plan_mm();
        // Model plans are in metres (or what they say); convert them.
        if state.source != Source::Selection {
            let from = mm_per_unit(if plan.units.is_empty() {
                "m"
            } else {
                &plan.units
            })
            .unwrap_or(1000.0);
            let to = drawing_mm.unwrap_or(1000.0);
            fp::scale(&mut plan, from / to);
        }
        fp::check(&plan)?;
        let wall_height = if state.build_3d {
            let h: f64 = state
                .wall_height
                .trim()
                .parse()
                .ok()
                .filter(|h: &f64| *h > 0.0)
                .ok_or("enter a positive wall height")?;
            let tallest = plan
                .openings
                .iter()
                .map(|o| o.sill + o.height)
                .fold(0.0, f64::max);
            if tallest > h + 1e-9 {
                return Err(format!(
                    "the wall height {} is lower than the tallest opening ({})",
                    fmt(h),
                    fmt(tallest)
                ));
            }
            Some(h)
        } else {
            None
        };
        let (lo, hi) = fp::extents(&plan);
        let gap = (hi[0] - lo[0]).max(hi[1] - lo[1]) * 0.2;
        let origin = match self.tabs[i].scene.model_space_extents() {
            Some((min, max)) if max.x > min.x => [max.x as f64 + gap - lo[0], min.y as f64 - lo[1]],
            _ => [0.0, 0.0],
        };
        let mut steps = fp::plan_2d(&plan, origin, drawing_mm, state.schedule);
        if let Some(h) = wall_height {
            let layer = self.tabs[i]
                .scene
                .document
                .header
                .current_layer_name
                .clone();
            steps.extend(fp::plan_3d(
                &plan,
                origin,
                h,
                if layer.is_empty() { "0" } else { &layer },
            ));
        }
        let rooms = plan.rooms.len();
        let total: f64 = plan
            .rooms
            .iter()
            .map(|r| fp::area_perimeter(&r.points).0)
            .sum();
        self.apply_ai_plan(&StepPlan {
            summary: "floor plan".into(),
            message: None,
            steps,
        })?;
        Ok(format!(
            "drew {} wall(s), {} opening(s) and {rooms} room(s) ({}){}; one UNDO removes it.",
            plan.walls.len(),
            plan.openings.len(),
            fp::area_label(total, drawing_mm),
            if wall_height.is_some() {
                " with a 3D massing model"
            } else {
                ""
            }
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUSE: &str = r#"{"units":"m","wall_height":2.7,"wall_thickness":0.3,
      "walls":[{"a":[0,0],"b":[10,0]},{"a":[10,0],"b":[10,6]},{"a":[10,6],"b":[0,6]},{"a":[0,6],"b":[0,0]},
               {"a":[6,0],"b":[6,6],"t":0.1}],
      "openings":[{"kind":"door","wall":0,"at":2,"width":0.9},
                  {"kind":"door","wall":4,"at":3,"width":0.8},
                  {"kind":"window","wall":2,"at":7,"width":1.5,"sill":0.9,"height":1.2}],
      "rooms":[{"name":"Living","points":[[0,0],[6,0],[6,6],[0,6]]},{"name":"Bedroom","points":[[6,0],[10,0],[10,6],[6,6]]}]}"#;

    fn count(app: &mut OpenCADStudio, layer: &str) -> u64 {
        app.automation_op(&serde_json::json!({"op":"query","layer":layer}).to_string())["count"]
            .as_u64()
            .unwrap_or(0)
    }

    #[test]
    fn draws_a_house_in_millimetres_with_schedule_and_massing() {
        let mut app = OpenCADStudio::new_for_test();
        app.automation_op(r#"{"op":"new"}"#);
        let i = app.active_tab;
        app.tabs[i].scene.document.header.insertion_units = 4;
        app.open_ai_plan();
        let st = app.ai_plan.as_mut().unwrap();
        assert_eq!(st.wall_height, "2700");
        st.plan = Some(fp::parse_plan(HOUSE).unwrap());
        st.build_3d = true;
        let report = app.ai_plan_insert().expect("draws");
        assert!(report.contains("60 m²"), "{report}");
        assert_eq!(count(&mut app, fp::L_WALL), 8);
        assert_eq!(count(&mut app, fp::L_DOOR), 4);
        assert_eq!(count(&mut app, fp::L_GLAZ), 5);
        assert_eq!(count(&mut app, fp::L_AREA), 2);
        assert!(count(&mut app, fp::L_SCHD) > 10);
        assert_eq!(count(&mut app, fp::L_3D), 12);
        // Converted to millimetres: the living room boundary is 6000 × 6000.
        let area = app.automation_op(r#"{"op":"query","layer":"A-AREA","detail":"geometry"}"#);
        let v = &area["entities"][0]["vertices"];
        assert!(v.to_string().contains("6000"), "{v}");
        // A lintel sits on top of the door: its solid spans 2100 → 2700.
        let solids = app.automation_op(r#"{"op":"query","layer":"A-WALL-3D"}"#);
        let handles: Vec<String> = solids["entities"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["handle"].as_str().unwrap().to_string())
            .collect();
        let m = app.automation_op(
            &serde_json::json!({"protocol":1,"op":"measure","handles":handles}).to_string(),
        );
        let top = m["measurements"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x["bounds"]["max"][2].as_f64().unwrap())
            .fold(0.0, f64::max);
        let lintel_floor = m["measurements"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x["bounds"]["min"][2].as_f64().unwrap())
            .fold(0.0, f64::max);
        assert!((top - 2700.0).abs() < 0.01, "{top}");
        assert!((lintel_floor - 2100.0).abs() < 0.01, "{lintel_floor}");
        assert_eq!(app.tabs[i].scene.document.header.current_layer_name, "0");
        app.active_modal = None;
        app.automation_op(r#"{"op":"undo"}"#);
        assert_eq!(app.automation_op(r#"{"op":"entities"}"#)["total"], 0);
    }

    #[test]
    fn builds_walls_and_rooms_from_selected_lines() {
        let mut app = OpenCADStudio::new_for_test();
        app.automation_op(r#"{"op":"new"}"#);
        let i = app.active_tab;
        app.tabs[i].scene.document.header.insertion_units = 6; // metres
        for cmd in [
            "LINE 0,0 8,0",
            "LINE 8,0 8,5",
            "LINE 8,5 0,5",
            "LINE 0,5 0,0",
        ] {
            app.automation_op(&serde_json::json!({"op":"run","cmd":cmd}).to_string());
        }
        app.automation_op(r#"{"op":"select","type":"Line"}"#);
        app.open_ai_plan();
        assert_eq!(app.ai_plan.as_ref().unwrap().thickness, "0.2");
        app.on_ai_plan(AiPlanMsg::Source(Source::Selection));
        app.on_ai_plan(AiPlanMsg::FromSelection);
        let st = app.ai_plan.as_ref().unwrap();
        assert_eq!(
            st.plan.as_ref().map(|p| p.walls.len()),
            Some(4),
            "{:?}",
            st.status
        );
        app.ai_plan_insert().expect("draws");
        assert_eq!(count(&mut app, fp::L_WALL), 4);
    }
}
