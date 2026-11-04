//! Text to part (AIPART): asks the model for a parametric definition,
//! builds it with the kernel's solid commands through the plan applier, and
//! reopens a part made this way (from its stored definition) to rebuild it
//! with new sizes.

use super::{Message, ModalKind, OpenCADStudio};
use crate::ai::command_plan::Plan;
use crate::ai::part_gen::{self as pg, Part};
use crate::ai::{AiFeature, ChatMessage, ChatRequest, ChatResponse};
use crate::ui::window::ai_part::{AiPartMsg, State};
use iced::Task;
use serde_json::{json, Value};

impl OpenCADStudio {
    /// The stored definition on `handle`, if it is an AIPART solid.
    pub(in crate::app) fn ai_part_stored(
        &mut self,
        handle: codec::Handle,
    ) -> Option<(Part, [f64; 3])> {
        let hex = format!("{:X}", handle.value());
        let reply = self.ai_automation_read(&json!({
            "protocol": 1,
            "op": "xdata_get",
            "handles": [hex],
            "app": pg::XDATA_APP,
        }));
        let strings: Vec<String> = reply["items"][0]["xdata"][pg::XDATA_APP]
            .as_array()?
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect();
        pg::from_xdata(&strings).ok()
    }

    fn ai_automation_read(&mut self, request: &Value) -> Value {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.automation_op(&request.to_string())
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = request;
            Value::Null
        }
    }

    pub(in crate::app) fn open_ai_part(&mut self) {
        let i = self.active_tab;
        if self.tabs[i].is_start {
            self.command_line
                .push_error("Open a drawing or start a new one before using AIPART.");
            return;
        }
        let selected = self.tabs[i].scene.selected_handles_in_order();
        let mut state = State::new();
        if let [one] = selected.as_slice() {
            if let Some((part, origin)) = self.ai_part_stored(*one) {
                state.editing = Some(*one);
                state.origin = Some(origin);
                state.status = Some(Ok(format!(
                    "Loaded \"{}\" from the selected solid.",
                    part.name
                )));
                state.set_part(part);
            }
        }
        // Keep an unfinished new part when the dialog is reopened.
        if state.editing.is_none() {
            if let Some(prev) = self.ai_part.take().filter(|p| p.editing.is_none()) {
                state = prev;
            }
        }
        self.ai_part = Some(state);
        self.active_modal = Some(ModalKind::AiPart);
        self.reset_modal_geometry();
        self.ai_part_estimate();
    }

    fn ai_part_request(&self) -> Option<ChatRequest> {
        let state = self.ai_part.as_ref()?;
        let units = self.ai_drawing_units().unwrap_or("mm");
        let mut messages = vec![ChatMessage::system(pg::system_prompt(units))];
        if let Some(part) = &state.part {
            // "Generate again" refines the current definition.
            messages.push(ChatMessage::assistant(part.to_json().to_string()));
        }
        messages.push(ChatMessage::user(state.prompt.trim()));
        Some(ChatRequest {
            messages,
            temperature: Some(0.1),
            json_mode: true,
        })
    }

    fn ai_part_estimate(&mut self) {
        let (settings, keys) = crate::ai::settings::current();
        let estimate = match settings.route(AiFeature::TextToPart, &keys) {
            Ok(route) => self
                .ai_part_request()
                .map(|r| crate::ai::describe_estimate(&route, &r))
                .unwrap_or_default(),
            Err(e) => e,
        };
        if let Some(state) = self.ai_part.as_mut() {
            state.estimate = estimate;
        }
    }

    pub(in crate::app) fn on_ai_part(&mut self, msg: AiPartMsg) -> Task<Message> {
        let Some(state) = self.ai_part.as_mut() else {
            return Task::none();
        };
        match msg {
            AiPartMsg::Prompt(v) => {
                state.prompt = v;
                state.confirm_armed = false;
            }
            AiPartMsg::Param(n, text) => {
                if let Some(slot) = state.fields.get_mut(n) {
                    *slot = text.clone();
                }
                let name = state
                    .part
                    .as_ref()
                    .and_then(|p| p.params.get(n))
                    .map(|p| p.name.clone());
                if let (Some(part), Some(name)) = (state.part.as_mut(), name) {
                    let result = part.set_param(&name, &text);
                    if let Some(e) = state.field_errors.get_mut(n) {
                        *e = result.err();
                    }
                }
                return Task::none();
            }
            AiPartMsg::Generate => return self.ai_part_generate(),
            AiPartMsg::Reply(result) => return self.ai_part_reply(result),
            AiPartMsg::Insert => {
                match self.ai_part_insert() {
                    Ok(report) => {
                        self.command_line.push_info(&format!("AIPART: {report}"));
                        self.ai_part = None;
                        self.active_modal = None;
                    }
                    Err(e) => {
                        if let Some(state) = self.ai_part.as_mut() {
                            state.status = Some(Err(e));
                        }
                    }
                }
                return Task::none();
            }
            AiPartMsg::OpenSettings => {
                self.open_ai_settings();
                return Task::none();
            }
        }
        self.ai_part_estimate();
        Task::none()
    }

    fn ai_part_generate(&mut self) -> Task<Message> {
        let (settings, keys) = crate::ai::settings::current();
        let route = match settings.route(AiFeature::TextToPart, &keys) {
            Ok(r) => r,
            Err(e) => {
                if let Some(state) = self.ai_part.as_mut() {
                    state.status = Some(Err(format!("{e}. Open Settings to choose a model.")));
                }
                return Task::none();
            }
        };
        let armed = self.ai_part.as_ref().is_some_and(|s| s.confirm_armed);
        if settings.confirm_before_send && !route.is_local && !armed {
            self.ai_part_estimate();
            if let Some(state) = self.ai_part.as_mut() {
                state.confirm_armed = true;
            }
            return Task::none();
        }
        let Some(request) = self.ai_part_request() else {
            return Task::none();
        };
        if let Some(state) = self.ai_part.as_mut() {
            state.busy = true;
            state.confirm_armed = false;
            state.retries = 0;
            state.status = None;
        }
        Self::ai_part_send(route, request)
    }

    fn ai_part_send(route: crate::ai::ResolvedRoute, request: ChatRequest) -> Task<Message> {
        Task::perform(
            crate::ai::run_blocking(move || crate::ai::client::chat(&route, &request)),
            |r| Message::AiPart(AiPartMsg::Reply(r)),
        )
    }

    fn ai_part_reply(&mut self, result: Result<ChatResponse, String>) -> Task<Message> {
        let (settings, keys) = crate::ai::settings::current();
        let request = self.ai_part_request();
        let Some(state) = self.ai_part.as_mut() else {
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
        match pg::parse_part(&response.content) {
            Ok(part) => {
                state.status = Some(Ok(format!(
                    "\"{}\" with {} parameter(s). Adjust the sizes if needed, then Insert.",
                    part.name,
                    part.params.len()
                )));
                state.set_part(part);
                Task::none()
            }
            Err(e) if state.retries == 0 && !e.contains("cannot") => {
                state.retries = 1;
                state.status = Some(Err(format!("{e} — asking the model again…")));
                let (Ok(route), Some(mut request)) =
                    (settings.route(AiFeature::TextToPart, &keys), request)
                else {
                    return Task::none();
                };
                request
                    .messages
                    .push(ChatMessage::assistant(response.content));
                request.messages.push(ChatMessage::user(format!(
                    "That definition could not be used: {e}. Reply again with only the corrected JSON."
                )));
                state.busy = true;
                Self::ai_part_send(route, request)
            }
            Err(e) => {
                state.status = Some(Err(e));
                Task::none()
            }
        }
    }

    /// Build (or rebuild) the part as one undo step.
    pub(in crate::app) fn ai_part_insert(&mut self) -> Result<String, String> {
        let i = self.active_tab;
        let state = self.ai_part.as_ref().ok_or("nothing to insert")?;
        let mut part = state.part.clone().ok_or("generate a part first")?;
        // Convert a part designed in other units into the drawing's.
        if let (Some(from), Some(to)) = (
            crate::ai::image_to_model::mm_per_unit(&part.units),
            self.ai_drawing_units()
                .and_then(crate::ai::image_to_model::mm_per_unit),
        ) {
            if (from - to).abs() > 1e-12 {
                return Err(format!(
                    "the part is in {} but the drawing is in {}; generate it again in the drawing's units",
                    part.units,
                    self.ai_drawing_units().unwrap_or("?")
                ));
            }
        }
        if part.units.is_empty() {
            part.units = self.ai_drawing_units().unwrap_or("").to_string();
        }
        let editing = state.editing;
        let origin = match state.origin {
            Some(o) => o,
            None => {
                let size = pg::instances(&part)
                    .map(|insts| {
                        let (lo, hi) = pg::extents(&insts);
                        (hi[0] - lo[0]).max(hi[1] - lo[1])
                    })
                    .unwrap_or(10.0);
                match self.tabs[i].scene.model_space_extents() {
                    Some((min, max)) if max.x > min.x => {
                        [max.x as f64 + size, min.y as f64 + size / 2.0, 0.0]
                    }
                    _ => [0.0, 0.0, 0.0],
                }
            }
        };
        let layer = self.tabs[i]
            .scene
            .document
            .header
            .current_layer_name
            .clone();
        let mut steps = Vec::new();
        if let Some(old) = editing {
            steps.push(json!({"op": "entities_delete", "handles": [format!("{:X}", old.value())], "note": "remove the previous solid"}));
        }
        steps.extend(pg::plan(
            &part,
            origin,
            if layer.is_empty() { "0" } else { &layer },
        )?);
        let plan = Plan {
            summary: part.name.clone(),
            message: None,
            steps,
        };
        self.apply_ai_plan(&plan)?;
        Ok(format!(
            "{} \"{}\" on layer {}; select it and run AIPART to change its sizes. One UNDO removes it.",
            if editing.is_some() { "rebuilt" } else { "inserted" },
            part.name,
            pg::LAYER
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::OpenCADStudio;
    use crate::ai::part_gen::parse_part;

    fn solids(app: &mut OpenCADStudio) -> Vec<String> {
        app.automation_op(r#"{"op":"query","type":"3D Solid"}"#)["entities"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|e| e["handle"].as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn measure(app: &mut OpenCADStudio, h: &str) -> (f64, [f64; 3]) {
        let m = app.automation_op(
            &serde_json::json!({"protocol":1,"op":"measure","handles":[h]}).to_string(),
        );
        let r = &m["measurements"][0];
        let size = |k: usize| {
            r["bounds"]["max"][k].as_f64().unwrap() - r["bounds"]["min"][k].as_f64().unwrap()
        };
        (
            r["mesh"]["volume"].as_f64().unwrap(),
            [size(0), size(1), size(2)],
        )
    }

    #[test]
    fn builds_a_flange_with_a_revolved_hub_and_patterned_holes_then_rebuilds_it() {
        let mut app = OpenCADStudio::new_for_test();
        app.automation_op(r#"{"op":"new"}"#);
        let i = app.active_tab;
        app.tabs[i].scene.document.header.insertion_units = 4;
        // Something selected must not confuse the interactive steps.
        app.automation_op(r#"{"op":"run","cmd":"CIRCLE 500,500 1"}"#);
        app.automation_op(r#"{"op":"select","type":"Circle"}"#);
        let part = parse_part(
            r#"{"name":"flange","units":"mm","parameters":{"D":80,"t":10,"hub":"D/2","n":4,"hole":8},
            "bodies":[
              {"name":"disc","op":"add","cylinder":{"d":"D","h":"t"}},
              {"name":"hub","op":"add","revolve":{"points":[[0,0],["hub/2",0],["hub/2",20],[0,20]]},"at":[0,0,"t"]},
              {"name":"bore","op":"cut","cylinder":{"d":10,"h":100},"at":[0,0,-1]},
              {"name":"bolt holes","op":"cut","cylinder":{"d":"hole","h":"t"},"pattern":{"circular":{"count":"n","radius":30}}}
            ]}"#,
        )
        .unwrap();
        app.open_ai_part();
        app.ai_part.as_mut().unwrap().set_part(part);
        let report = app.ai_part_insert().expect("builds");
        assert!(report.contains("inserted"), "{report}");
        let s = solids(&mut app);
        assert_eq!(s.len(), 1, "one finished solid: {s:?}");
        let (vol, size) = measure(&mut app, &s[0]);
        assert!(
            (size[0] - 80.0).abs() < 0.05 && (size[1] - 80.0).abs() < 0.05,
            "{size:?}"
        );
        assert!((size[2] - 30.0).abs() < 0.01, "disc + hub height: {size:?}");
        let pi = std::f64::consts::PI;
        let expected = pi * 40.0f64.powi(2) * 10.0 + pi * 20.0f64.powi(2) * 20.0
            - pi * 5.0f64.powi(2) * 30.0
            - 4.0 * pi * 4.0f64.powi(2) * 10.0;
        assert!(
            (vol - expected).abs() / expected < 0.01,
            "volume {vol} vs {expected}"
        );
        let layer = app.automation_op(r#"{"op":"query","type":"3D Solid"}"#)["entities"][0]
            ["layer"]
            .clone();
        assert_eq!(layer, "AI-PART");
        assert_eq!(
            app.tabs[i].scene.document.header.current_layer_name, "0",
            "current layer restored"
        );

        // Reopen it from the solid, make it thicker, rebuild.
        let h = codec::Handle::new(u64::from_str_radix(&s[0], 16).unwrap());
        app.tabs[i].scene.deselect_all();
        app.tabs[i].scene.select_entity(h, false);
        app.open_ai_part();
        let st = app.ai_part.as_mut().unwrap();
        assert_eq!(st.editing, Some(h), "opened in edit mode");
        st.part.as_mut().unwrap().set_param("t", "15").unwrap();
        app.ai_part_insert().expect("rebuilds");
        let s2 = solids(&mut app);
        assert_eq!(s2.len(), 1);
        assert_ne!(s2[0], s[0], "the old solid was replaced");
        let (_, size2) = measure(&mut app, &s2[0]);
        assert!((size2[2] - 35.0).abs() < 0.01, "{size2:?}");
        // Rebuilt solids keep their definition too.
        let h2 = codec::Handle::new(u64::from_str_radix(&s2[0], 16).unwrap());
        let (stored, _) = app.ai_part_stored(h2).expect("definition stored");
        assert_eq!(stored.vars()["t"], 15.0);
        // One undo goes back to the first solid.
        app.active_modal = None;
        app.automation_op(r#"{"op":"undo"}"#);
        assert_eq!(solids(&mut app), s);
    }

    #[test]
    fn builds_a_flanged_tee_with_bodies_along_x_and_y() {
        let mut app = OpenCADStudio::new_for_test();
        app.automation_op(r#"{"op":"new"}"#);
        let i = app.active_tab;
        app.tabs[i].scene.document.header.insertion_units = 4;
        let def = std::env::var("OCS_TEE_JSON")
            .ok()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .unwrap_or_else(|| {
                r#"{"name":"tee","units":"mm","parameters":{"OD":114.3,"ID":102.3,"L":400,"B":200,"ft":20,"FD":230.3,"zc":"FD/2"},
                "bodies":[
                  {"op":"add","cylinder":{"d":"OD","h":"L"},"axis":"x","at":["-L/2",0,"zc"]},
                  {"op":"add","cylinder":{"d":"OD","h":"B"},"axis":"y","at":[0,0,"zc"]},
                  {"op":"add","cylinder":{"d":"FD","h":"ft"},"axis":"y","at":[0,"B-ft","zc"]},
                  {"op":"cut","cylinder":{"d":"ID","h":"L+2"},"axis":"x","at":["-L/2-1",0,"zc"]},
                  {"op":"cut","cylinder":{"d":"ID","h":"B+1"},"axis":"y","at":[0,0,"zc"]},
                  {"op":"cut","cylinder":{"d":22,"h":"ft+2"},"axis":"y","at":[0,"B-ft-1","zc"],
                   "pattern":{"circular":{"count":8,"radius":95,"start_deg":22.5}}}]}"#
                    .to_string()
            });
        let part = parse_part(&def).unwrap();
        app.open_ai_part();
        let st = app.ai_part.as_mut().unwrap();
        st.set_part(part);
        st.origin = Some([0.0, 0.0, 0.0]);
        app.ai_part_insert().expect("builds");
        let s = solids(&mut app);
        assert_eq!(s.len(), 1, "one finished solid: {s:?}");
        let m = app.automation_op(
            &serde_json::json!({"protocol":1,"op":"measure","handles":[s[0]]}).to_string(),
        );
        let r = &m["measurements"][0];
        eprintln!(
            "TEE volume {} bounds {} .. {}",
            r["mesh"]["volume"], r["bounds"]["min"], r["bounds"]["max"]
        );
        let lo: Vec<f64> = (0..3)
            .map(|k| r["bounds"]["min"][k].as_f64().unwrap())
            .collect();
        let hi: Vec<f64> = (0..3)
            .map(|k| r["bounds"]["max"][k].as_f64().unwrap())
            .collect();
        assert!(
            (lo[0] + 200.0).abs() < 0.1 && (hi[0] - 200.0).abs() < 0.1,
            "run along X: {lo:?} {hi:?}"
        );
        assert!((hi[1] - 200.0).abs() < 0.1, "branch along +Y: {hi:?}");
        assert!(
            lo[2].abs() < 0.1 && (hi[2] - 230.3).abs() < 0.2,
            "rests on z=0: {lo:?} {hi:?}"
        );
        if let Ok(out) = std::env::var("OCS_TEE_OUT") {
            let saved = app.automation_op(&serde_json::json!({"op":"save","path":out}).to_string());
            eprintln!("SAVE {saved}");
            let h = codec::Handle::new(u64::from_str_radix(&s[0], 16).unwrap());
            app.tabs[i].scene.restore_solid_models(&[h]);
            let body = app.tabs[i].scene.solid_models.get(&h).cloned().unwrap();
            let mesh = kernel::brep::mesh::tessellate(
                &body,
                kernel::brep::mesh::TessellationTolerance::new(
                    kernel::tessellation::DEFAULT_ANGLE,
                    1e-9,
                ),
            );
            let json = serde_json::json!({"positions": mesh.mesh.positions, "triangles": mesh.mesh.triangles});
            std::fs::write(format!("{out}.mesh.json"), json.to_string()).unwrap();
        }
    }

    /// Manual run: build `OCS_PART_JSON` at the origin and save it to
    /// `OCS_PART_OUT` (.dwg) with its triangles in `<out>.mesh.json`.
    #[test]
    fn manual_part_from_env() {
        let (Ok(def), Ok(out)) = (
            std::env::var("OCS_PART_JSON"),
            std::env::var("OCS_PART_OUT"),
        ) else {
            return;
        };
        let mut app = OpenCADStudio::new_for_test();
        app.automation_op(r#"{"op":"new"}"#);
        let i = app.active_tab;
        app.tabs[i].scene.document.header.insertion_units = 4;
        let part = parse_part(&std::fs::read_to_string(def).unwrap()).unwrap();
        app.open_ai_part();
        let st = app.ai_part.as_mut().unwrap();
        st.set_part(part);
        st.origin = Some([0.0, 0.0, 0.0]);
        eprintln!("{}", app.ai_part_insert().expect("builds"));
        let s = solids(&mut app);
        let (vol, size) = measure(&mut app, &s[0]);
        eprintln!("PART {} solid(s), volume {vol:.1}, size {size:?}", s.len());
        let h = codec::Handle::new(u64::from_str_radix(&s[0], 16).unwrap());
        app.tabs[i].scene.restore_solid_models(&[h]);
        let body = app.tabs[i].scene.solid_models.get(&h).cloned().unwrap();
        let mesh = kernel::brep::mesh::tessellate(
            &body,
            kernel::brep::mesh::TessellationTolerance::new(
                kernel::tessellation::DEFAULT_ANGLE,
                1e-9,
            ),
        );
        let json =
            serde_json::json!({"positions": mesh.mesh.positions, "triangles": mesh.mesh.triangles});
        std::fs::write(format!("{out}.mesh.json"), json.to_string()).unwrap();
        eprintln!(
            "{}",
            app.automation_op(&serde_json::json!({"op":"save","path":out}).to_string())
        );
    }
}
