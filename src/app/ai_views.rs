//! Drawing views from a 3D solid (AIVIEWS): tessellates the selected solid
//! with the kernel, projects the chosen views with hidden-line removal, and
//! inserts them — with a basic or AI-chosen dimension scheme — as one undo
//! step through the shared plan applier.

use super::{Message, ModalKind, OpenCADStudio};
use crate::ai::command_plan::Plan as StepPlan;
use crate::ai::drawing_views::{self as dv, SolidData, ViewKind};
use crate::ai::{AiFeature, ChatMessage, ChatRequest};
use crate::ui::window::ai_views::{AiViewsMsg, DimMode, State};
use iced::Task;

/// The dimension style AIVIEWS dimensions use.
const DIM_STYLE: &str = "AI-VIEWS";

impl OpenCADStudio {
    /// The solid's triangles, exact edges and per-view silhouettes.
    fn ai_views_solid(
        &mut self,
        h: codec::Handle,
    ) -> Result<(SolidData, Vec<(ViewKind, Vec<[[f64; 3]; 2]>)>), String> {
        let i = self.active_tab;
        self.tabs[i].scene.restore_solid_models(&[h]);
        let body = self.tabs[i]
            .scene
            .solid_models
            .get(&h)
            .cloned()
            .ok_or("the selected solid has no B-rep the kernel can read")?;
        let mesh = kernel::brep::mesh::tessellate(
            &body,
            kernel::brep::mesh::TessellationTolerance::new(
                kernel::tessellation::DEFAULT_ANGLE,
                1e-9,
            ),
        );
        if mesh.mesh.triangles.is_empty() {
            return Err("the solid could not be tessellated".into());
        }
        let pos = &mesh.mesh.positions;
        let data = SolidData {
            triangles: mesh
                .mesh
                .triangles
                .iter()
                .map(|t| [pos[t[0]], pos[t[1]], pos[t[2]]])
                .collect(),
            normals: mesh
                .mesh
                .triangles
                .iter()
                .map(|t| {
                    [
                        mesh.mesh.normals[t[0]],
                        mesh.mesh.normals[t[1]],
                        mesh.mesh.normals[t[2]],
                    ]
                })
                .collect(),
            edges: mesh
                .drawing_edges
                .iter()
                .map(|e| e.positions.clone())
                .collect(),
        };
        let source = mesh.silhouette_source();
        let silhouettes = ViewKind::ALL
            .iter()
            .map(|k| {
                let flat = kernel::brep::mesh::silhouette(&source, k.frame().0);
                (*k, flat.chunks_exact(2).map(|p| [p[0], p[1]]).collect())
            })
            .collect();
        Ok((data, silhouettes))
    }

    fn ai_views_recompute(&mut self) {
        let Some(state) = self.ai_views.as_mut() else {
            return;
        };
        state.views = ViewKind::ALL
            .iter()
            .filter(|k| state.enabled.contains(k))
            .map(|k| {
                let sil = state
                    .silhouettes
                    .iter()
                    .find(|(kk, _)| kk == k)
                    .map(|(_, s)| s.as_slice())
                    .unwrap_or(&[]);
                dv::project(*k, &state.data, sil, state.hidden)
            })
            .collect();
        state.ai_dims = None;
    }

    pub(in crate::app) fn open_ai_views(&mut self) {
        let i = self.active_tab;
        if self.tabs[i].is_start {
            self.command_line
                .push_error("Open a drawing before using AIVIEWS.");
            return;
        }
        let solids: Vec<codec::Handle> = self.tabs[i]
            .scene
            .selected_handles_in_order()
            .into_iter()
            .filter(|h| {
                matches!(
                    self.tabs[i].scene.document.get_entity(*h),
                    Some(codec::EntityType::Solid3D(_))
                )
            })
            .collect();
        let [solid] = solids.as_slice() else {
            self.command_line
                .push_error("AIVIEWS: select one 3D solid first.");
            return;
        };
        let (data, silhouettes) = match self.ai_views_solid(*solid) {
            Ok(v) => v,
            Err(e) => {
                self.command_line.push_error(&format!("AIVIEWS: {e}"));
                return;
            }
        };
        self.ai_views = Some(State {
            solid: *solid,
            data,
            silhouettes,
            enabled: vec![
                ViewKind::Front,
                ViewKind::Top,
                ViewKind::Right,
                ViewKind::Iso,
            ],
            hidden: true,
            dim_mode: DimMode::Basic,
            standard: "ISO 129-1".into(),
            views: Vec::new(),
            ai_dims: None,
            busy: false,
            status: None,
            estimate: String::new(),
            confirm_armed: false,
        });
        self.ai_views_recompute();
        self.active_modal = Some(ModalKind::AiViews);
        self.reset_modal_geometry();
        self.ai_views_estimate();
    }

    fn ai_views_request(&self) -> Option<ChatRequest> {
        let state = self.ai_views.as_ref()?;
        Some(ChatRequest {
            messages: vec![
                ChatMessage::system(dv::dims_prompt(&state.views, &state.standard)),
                ChatMessage::user("Choose the dimensions."),
            ],
            temperature: Some(0.1),
            json_mode: true,
        })
    }

    fn ai_views_estimate(&mut self) {
        let mode = self.ai_views.as_ref().map(|s| s.dim_mode);
        let estimate = if mode == Some(DimMode::Ai) {
            let (settings, keys) = crate::ai::settings::current();
            match settings.route(AiFeature::AutoDimension, &keys) {
                Ok(route) => self
                    .ai_views_request()
                    .map(|r| crate::ai::describe_estimate(&route, &r))
                    .unwrap_or_default(),
                Err(e) => e,
            }
        } else {
            "Views and basic dimensions are computed locally — no model is used.".into()
        };
        if let Some(state) = self.ai_views.as_mut() {
            state.estimate = estimate;
        }
    }

    /// Spacing of dimension lines from the views, in drawing units.
    fn ai_views_spacing(views: &[dv::View]) -> f64 {
        let size = views
            .iter()
            .map(|v| v.width().max(v.height()))
            .fold(0.0, f64::max);
        (size / 12.0).max(1e-6)
    }

    pub(in crate::app) fn on_ai_views(&mut self, msg: AiViewsMsg) -> Task<Message> {
        let Some(state) = self.ai_views.as_mut() else {
            return Task::none();
        };
        match msg {
            AiViewsMsg::Toggle(k, on) => {
                state.enabled.retain(|x| *x != k);
                if on {
                    state.enabled.push(k);
                }
                self.ai_views_recompute();
            }
            AiViewsMsg::Hidden(v) => {
                state.hidden = v;
                self.ai_views_recompute();
            }
            AiViewsMsg::Dims(m) => {
                state.dim_mode = m;
                state.confirm_armed = false;
            }
            AiViewsMsg::Standard(s) => {
                state.standard = s;
                state.ai_dims = None;
            }
            AiViewsMsg::Suggest => return self.ai_views_suggest(),
            AiViewsMsg::Reply(r) => {
                state.busy = false;
                let spacing = Self::ai_views_spacing(&state.views);
                match r.and_then(|resp| dv::parse_dims(&resp.content, &state.views, spacing)) {
                    Ok(dims) => {
                        state.status = Some(Ok(format!(
                            "The model chose {} dimension(s); sizes are measured from the views.",
                            dims.len()
                        )));
                        state.ai_dims = Some(dims);
                    }
                    Err(e) => state.status = Some(Err(e)),
                }
            }
            AiViewsMsg::Insert => {
                match self.ai_views_insert() {
                    Ok(report) => {
                        self.command_line.push_info(&format!("AIVIEWS: {report}"));
                        self.ai_views = None;
                        self.active_modal = None;
                    }
                    Err(e) => {
                        if let Some(state) = self.ai_views.as_mut() {
                            state.status = Some(Err(e));
                        }
                    }
                }
                return Task::none();
            }
            AiViewsMsg::OpenSettings => {
                self.open_ai_settings();
                return Task::none();
            }
        }
        self.ai_views_estimate();
        Task::none()
    }

    fn ai_views_suggest(&mut self) -> Task<Message> {
        let (settings, keys) = crate::ai::settings::current();
        let route = match settings.route(AiFeature::AutoDimension, &keys) {
            Ok(r) => r,
            Err(e) => {
                if let Some(state) = self.ai_views.as_mut() {
                    state.status = Some(Err(format!("{e}. Open Settings to choose a model.")));
                }
                return Task::none();
            }
        };
        let armed = self.ai_views.as_ref().is_some_and(|s| s.confirm_armed);
        if settings.confirm_before_send && !route.is_local && !armed {
            self.ai_views_estimate();
            if let Some(state) = self.ai_views.as_mut() {
                state.confirm_armed = true;
            }
            return Task::none();
        }
        let Some(request) = self.ai_views_request() else {
            return Task::none();
        };
        if let Some(state) = self.ai_views.as_mut() {
            state.busy = true;
            state.confirm_armed = false;
            state.status = None;
        }
        Task::perform(
            crate::ai::run_blocking(move || crate::ai::client::chat(&route, &request)),
            |r| Message::AiViews(AiViewsMsg::Reply(r)),
        )
    }

    /// Insert the views (and dimensions) to the right of the drawing.
    /// Make `AI-VIEWS` (the current style scaled so its text is
    /// `text_height`) current; returns the style that was current.
    fn ai_views_use_dim_style(&mut self, text_height: f64, asme: bool) -> (String, codec::Handle) {
        let i = self.active_tab;
        let doc = &mut self.tabs[i].scene.document;
        let previous = (
            doc.header.current_dimstyle_name.clone(),
            doc.header.current_dimstyle_handle,
        );
        let base = doc
            .dim_styles
            .iter()
            .find(|s| s.name.eq_ignore_ascii_case(previous.0.trim()))
            .or_else(|| doc.dim_styles.iter().next())
            .cloned();
        let mut style = base.unwrap_or_else(|| codec::tables::DimStyle::new(DIM_STYLE));
        let txt = if style.dimtxt > 1e-9 {
            style.dimtxt
        } else {
            2.5
        };
        style.dimscale = text_height / txt;
        if asme {
            // ASME Y14.5 uses a decimal point.
            style.dimdsep = i16::from(b'.');
        }
        style.name = DIM_STYLE.to_string();
        let handle = match doc.dim_styles.get(DIM_STYLE) {
            Some(existing) => existing.handle,
            None => doc.allocate_handle(),
        };
        style.handle = handle;
        doc.dim_styles.add_or_replace(style);
        doc.header.current_dimstyle_name = DIM_STYLE.to_string();
        doc.header.current_dimstyle_handle = handle;
        crate::scene::creation_style::sync_header_dim_vars(doc);
        previous
    }

    pub(in crate::app) fn ai_views_insert(&mut self) -> Result<String, String> {
        let i = self.active_tab;
        let state = self.ai_views.as_ref().ok_or("nothing to insert")?;
        if state.views.is_empty() {
            return Err("tick at least one view".into());
        }
        let size = state
            .views
            .iter()
            .map(|v| v.width().max(v.height()))
            .fold(0.0, f64::max);
        let spacing = Self::ai_views_spacing(&state.views);
        let gap = size * 0.25 + spacing * 5.0;
        let origin = match self.tabs[i].scene.model_space_extents() {
            Some((min, max)) if max.x > min.x => [max.x as f64 + gap, min.y as f64],
            _ => [0.0, 0.0],
        };
        let offsets = dv::layout(&state.views, origin, gap);
        let text_height = spacing * 0.5;
        let mut steps = dv::views_steps(&state.views, &offsets, text_height);
        let dims = match state.dim_mode {
            DimMode::None => Vec::new(),
            DimMode::Basic => dv::basic_dims(&state.views, spacing),
            DimMode::Ai => state
                .ai_dims
                .clone()
                .ok_or("press Suggest for the AI dimension scheme first")?,
        };
        let layer = self.tabs[i]
            .scene
            .document
            .header
            .current_layer_name
            .clone();
        let dim_count = dims.len();
        steps.extend(dv::dims_steps(
            &dims,
            &state.views,
            &offsets,
            if layer.is_empty() { "0" } else { &layer },
        ));
        let n = state.views.len();
        // Dimension text in proportion to the views: a copy of the current
        // style with its DIMSCALE set, current only while the plan runs.
        let asme = state.dim_mode == DimMode::Ai && state.standard.starts_with("ASME");
        let previous =
            (dim_count > 0).then(|| self.ai_views_use_dim_style(text_height * 0.6, asme));
        let result = self.apply_ai_plan(&StepPlan {
            summary: "drawing views".into(),
            message: None,
            steps,
        });
        if let Some((name, handle)) = previous {
            let doc = &mut self.tabs[i].scene.document;
            doc.header.current_dimstyle_name = name;
            doc.header.current_dimstyle_handle = handle;
            crate::scene::creation_style::sync_header_dim_vars(doc);
        }
        result?;
        Ok(format!(
            "inserted {n} view(s){} on AI-VIEW layers; one UNDO removes them.",
            if dim_count > 0 {
                format!(" and {dim_count} dimension(s)")
            } else {
                String::new()
            }
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plate_with_hole(app: &mut OpenCADStudio) -> codec::Handle {
        app.automation_op(r#"{"op":"new"}"#);
        let i = app.active_tab;
        app.tabs[i].scene.document.header.insertion_units = 4;
        let part = crate::ai::part_gen::parse_part(
            r#"{"bodies":[{"op":"add","box":{"w":40,"d":20,"h":10}},{"op":"cut","cylinder":{"d":8,"h":10},"at":[5,0,0]}]}"#,
        )
        .unwrap();
        let steps = crate::ai::part_gen::plan(&part, [0.0, 0.0, 0.0], "0").unwrap();
        app.apply_ai_plan(&StepPlan {
            summary: String::new(),
            message: None,
            steps,
        })
        .unwrap();
        let q = app.automation_op(r#"{"op":"query","type":"3D Solid"}"#);
        codec::Handle::new(
            u64::from_str_radix(q["entities"][0]["handle"].as_str().unwrap(), 16).unwrap(),
        )
    }

    #[test]
    fn views_of_a_plate_show_the_hole_as_a_circle_on_top_and_hidden_lines_in_front() {
        let mut app = OpenCADStudio::new_for_test();
        let h = plate_with_hole(&mut app);
        let i = app.active_tab;
        app.tabs[i].scene.select_entity(h, false);
        app.open_ai_views();
        let st = app.ai_views.as_ref().expect("dialog opened");
        let get = |k: ViewKind| st.views.iter().find(|v| v.kind == k).unwrap();
        let top = get(ViewKind::Top);
        let circles = top.circles();
        assert_eq!(circles.len(), 1, "{:?}", top.visible);
        assert!((circles[0].1 - 4.0).abs() < 1e-6);
        assert!((top.width() - 40.0).abs() < 1e-6 && (top.height() - 20.0).abs() < 1e-6);
        let front = get(ViewKind::Front);
        // The hole's two sides show as hidden vertical lines in the front.
        let hidden_verticals = front
            .hidden
            .iter()
            .filter(|s| matches!(s, dv::Shape2::Line { a, b } if (a[0] - b[0]).abs() < 1e-6 && (a[1] - b[1]).abs() > 9.9))
            .count();
        assert_eq!(hidden_verticals, 2, "{:?}", front.hidden);
        assert!(get(ViewKind::Iso).hidden.is_empty());

        let report = app.ai_views_insert().expect("inserts");
        assert!(report.contains("4 view(s)"), "{report}");
        let count = |app: &mut OpenCADStudio, layer: &str| {
            app.automation_op(&serde_json::json!({"op":"query","layer":layer}).to_string())["count"]
                .as_u64()
                .unwrap_or(0)
        };
        assert!(count(&mut app, dv::L_VISIBLE) > 10);
        assert_eq!(
            count(&mut app, dv::L_HIDDEN),
            4,
            "the hole is hidden in the front and right views"
        );
        assert_eq!(
            count(&mut app, dv::L_CENTER),
            2,
            "one centre mark on the top view"
        );
        let dims = app.automation_op(r#"{"op":"query","type":"Dimension","detail":"full"}"#);
        let values: Vec<f64> = dims["entities"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|d| {
                let p = &d["properties"];
                p.as_object()?.values().next()?["base"]["actual_measurement"].as_f64()
            })
            .collect();
        let has = |x: f64| values.iter().any(|v| (v - x).abs() < 1e-6);
        assert!(
            has(40.0) && has(10.0) && has(20.0),
            "overall sizes: {values:?}"
        );
        assert!(has(8.0), "hole diameter: {values:?}");
        assert!(
            has(25.0) && has(10.0),
            "hole located from the lower-left corner: {values:?}"
        );
        assert!(dims["entities"]
            .as_array()
            .unwrap()
            .iter()
            .all(|d| d["layer"] == dv::L_DIMS));
        assert_eq!(app.tabs[i].scene.document.header.current_layer_name, "0");
        app.active_modal = None;
        app.automation_op(r#"{"op":"undo"}"#);
        assert_eq!(count(&mut app, dv::L_VISIBLE), 0);
        assert_eq!(
            app.automation_op(r#"{"op":"query","type":"3D Solid"}"#)["count"],
            1,
            "the solid stays"
        );
    }

    #[test]
    fn an_ai_scheme_is_measured_from_the_views() {
        let mut app = OpenCADStudio::new_for_test();
        let h = plate_with_hole(&mut app);
        let i = app.active_tab;
        app.tabs[i].scene.select_entity(h, false);
        app.open_ai_views();
        app.on_ai_views(AiViewsMsg::Dims(DimMode::Ai));
        assert!(app.ai_views_insert().is_err(), "needs a suggestion first");
        let st = app.ai_views.as_mut().unwrap();
        let spacing = OpenCADStudio::ai_views_spacing(&st.views);
        let reply = r#"{"dimensions":[{"view":"top","kind":"diameter","circle":"c0"},{"view":"top","kind":"horizontal","a":"p0","b":"p1"}]}"#;
        st.ai_dims = Some(dv::parse_dims(reply, &st.views, spacing).unwrap());
        app.ai_views_insert().expect("inserts");
        let dims = app.automation_op(r#"{"op":"query","type":"Dimension"}"#);
        assert_eq!(dims["count"], 2, "{dims}");
    }

    /// Manual run: build a part definition from `OCS_PART_JSON`, make its
    /// views and either write the dimension prompt (`OCS_VIEWS_PROMPT`) or
    /// apply a scheme reply (`OCS_VIEWS_REPLY`), then save to `OCS_VIEWS_OUT`.
    #[test]
    fn manual_views_of_a_part_from_env() {
        let Ok(def) = std::env::var("OCS_PART_JSON") else {
            return;
        };
        let mut app = OpenCADStudio::new_for_test();
        app.automation_op(r#"{"op":"new"}"#);
        let i = app.active_tab;
        app.tabs[i].scene.document.header.insertion_units = 4;
        let part = crate::ai::part_gen::parse_part(&std::fs::read_to_string(def).unwrap()).unwrap();
        let steps = crate::ai::part_gen::plan(&part, [0.0, 0.0, 0.0], "0").unwrap();
        app.apply_ai_plan(&StepPlan {
            summary: String::new(),
            message: None,
            steps,
        })
        .unwrap();
        let q = app.automation_op(r#"{"op":"query","type":"3D Solid"}"#);
        let h = codec::Handle::new(
            u64::from_str_radix(q["entities"][0]["handle"].as_str().unwrap(), 16).unwrap(),
        );
        app.tabs[i].scene.deselect_all();
        app.tabs[i].scene.select_entity(h, false);
        app.open_ai_views();
        let standard = std::env::var("OCS_VIEWS_STANDARD").unwrap_or("ASME Y14.5".into());
        let st = app.ai_views.as_mut().unwrap();
        st.standard = standard.clone();
        let spacing = OpenCADStudio::ai_views_spacing(&st.views);
        if let Ok(path) = std::env::var("OCS_VIEWS_PROMPT") {
            std::fs::write(path, dv::dims_prompt(&st.views, &standard)).unwrap();
        }
        if let Ok(path) = std::env::var("OCS_VIEWS_REPLY") {
            let reply = std::fs::read_to_string(path).unwrap();
            st.dim_mode = DimMode::Ai;
            st.ai_dims = Some(dv::parse_dims(&reply, &st.views, spacing).unwrap());
            eprintln!("AI DIMS {}", st.ai_dims.as_ref().unwrap().len());
        }
        eprintln!("{}", app.ai_views_insert().unwrap());
        if let Ok(out) = std::env::var("OCS_VIEWS_OUT") {
            app.tabs[i].scene.deselect_all();
            app.automation_op(
                &serde_json::json!({"op":"entities_delete","handles":[q["entities"][0]["handle"]]})
                    .to_string(),
            );
            eprintln!(
                "{}",
                app.automation_op(&serde_json::json!({"op":"save","path":out}).to_string())
            );
        }
    }

    #[test]
    fn hidden_lines_are_dashed_and_dimensions_scale_with_the_views() {
        let mut app = OpenCADStudio::new_for_test();
        let h = plate_with_hole(&mut app);
        let i = app.active_tab;
        app.tabs[i].scene.select_entity(h, false);
        app.open_ai_views();
        app.ai_views_insert().expect("inserts");
        let doc = &app.tabs[i].scene.document;
        let hidden: Vec<_> = doc
            .entities()
            .filter(|e| e.common().layer == dv::L_HIDDEN)
            .collect();
        assert!(!hidden.is_empty());
        assert!(hidden.iter().all(|e| e.common().linetype == "HIDDEN"));
        assert!(doc.line_types.contains("HIDDEN"), "loaded from the catalog");
        let style = doc.dim_styles.get(DIM_STYLE).expect("style made");
        assert!(style.dimscale > 0.0 && (style.dimscale - 1.0).abs() > 1e-6);
        assert_ne!(
            doc.header.current_dimstyle_name, DIM_STYLE,
            "the user's current style is restored"
        );
    }
}
