//! Drawing checker (AICHECK): snapshots the drawing for the rules in
//! `crate::ai::drawing_check`, runs the optional AI review, and offers Show /
//! Erase per finding.

use super::{Message, ModalKind, OpenCADStudio};
use crate::ai::command_plan::Plan;
use crate::ai::drawing_check::{self as dc, CheckInput, Item, ItemKind, Standard, StandardKind};
use crate::ai::{AiFeature, ChatMessage, ChatRequest, ChatResponse};
use crate::ui::window::ai_check::{AiCheckMsg, State};
use codec::EntityType;
use iced::Task;
use serde_json::json;

/// Where an imported company standard is kept between sessions.
#[cfg(not(target_arch = "wasm32"))]
fn company_path() -> Option<std::path::PathBuf> {
    crate::config::config_dir().map(|d| d.join("drawing_standard.json"))
}

fn load_company() -> Option<Standard> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let text = std::fs::read_to_string(company_path()?).ok()?;
        Standard::parse(&text).ok()
    }
    #[cfg(target_arch = "wasm32")]
    {
        None
    }
}

#[cfg(not(target_arch = "wasm32"))]
async fn pick_company() -> Result<(String, Standard), String> {
    let handle = crate::sys::file_dialog()
        .set_title("Select a drawing standard (JSON)")
        .add_filter("Drawing standard", &["json"])
        .pick_file()
        .await
        .ok_or_else(|| "Cancelled".to_string())?;
    let path = crate::sys::handle_path(&handle);
    let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let standard = Standard::parse(&text)?;
    if let Some(dest) = company_path() {
        if let Some(dir) = dest.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        std::fs::write(&dest, &text).map_err(|e| format!("could not keep the standard: {e}"))?;
    }
    Ok((path.display().to_string(), standard))
}

#[cfg(target_arch = "wasm32")]
async fn pick_company() -> Result<(String, Standard), String> {
    Err(crate::ai::client::DESKTOP_ONLY.into())
}

fn hex(h: codec::Handle) -> String {
    format!("{:X}", h.value())
}

fn xy(v: &codec::types::Vector3) -> [f64; 2] {
    [v.x, v.y]
}

impl OpenCADStudio {
    pub(in crate::app) fn open_ai_check(&mut self) {
        if self.tabs[self.active_tab].is_start {
            self.command_line
                .push_error("Open a drawing before running the drawing checker.");
            return;
        }
        if self.ai_check.is_none() {
            self.ai_check = Some(State::new(load_company()));
        }
        self.active_modal = Some(ModalKind::AiCheck);
        self.reset_modal_geometry();
        self.ai_check_estimate();
    }

    /// A neutral snapshot of the active drawing for the rules.
    pub(in crate::app) fn ai_check_input(&self) -> CheckInput {
        let tab = &self.tabs[self.active_tab];
        let doc = &tab.scene.document;
        let model = doc.header.model_space_block_handle;
        let mm = self
            .ai_drawing_units()
            .and_then(crate::ai::image_to_model::mm_per_unit);
        let mut items = Vec::new();
        for e in doc.entities() {
            let common = e.common();
            let kind = match e {
                EntityType::Line(l) => ItemKind::Line {
                    a: xy(&l.start),
                    b: xy(&l.end),
                },
                EntityType::Circle(c) => ItemKind::Circle {
                    c: xy(&c.center),
                    r: c.radius,
                },
                EntityType::Arc(a) => ItemKind::Arc {
                    c: xy(&a.center),
                    r: a.radius,
                    start: a.start_angle,
                    end: a.end_angle,
                },
                EntityType::Text(t) => ItemKind::Text {
                    value: t.value.clone(),
                    height: t.height,
                },
                EntityType::MText(t) => ItemKind::Text {
                    value: t.value.clone(),
                    height: t.height,
                },
                EntityType::Dimension(d) => {
                    let base = d.base();
                    ItemKind::Dimension {
                        measurement: base.actual_measurement,
                        text: base.text_override().map(str::to_string),
                        associative: crate::scene::dimension_assoc::dimension_is_associative(
                            doc,
                            common.handle,
                        ),
                        kind: format!("{:?}", base.dimension_type),
                    }
                }
                other => ItemKind::Other(crate::entities::names::ui_name(other).to_string()),
            };
            let bounds =
                matches!(kind, ItemKind::Text { .. } | ItemKind::Dimension { .. }).then(|| {
                    let (min, max) = crate::scene::convert::tess::entity_bounds_in(doc, e);
                    ([min[0], min[1]], [max[0], max[1]])
                });
            let linetype = common.linetype.trim();
            items.push(Item {
                handle: hex(common.handle),
                kind,
                layer: common.layer.clone(),
                bylayer_color: matches!(common.color, codec::types::Color::ByLayer),
                bylayer_linetype: linetype.is_empty() || linetype.eq_ignore_ascii_case("ByLayer"),
                space: hex(common.owner_handle),
                paper_space: common.owner_handle != model,
                bounds: bounds
                    .filter(|(a, b)| a.iter().chain(b).all(|v| v.is_finite()) && b[0] >= a[0]),
            });
        }
        CheckInput {
            mm_per_unit: mm,
            layers: doc.layers.iter().map(|l| l.name.clone()).collect(),
            items,
        }
    }

    fn ai_check_review_request(&self) -> Option<ChatRequest> {
        let state = self.ai_check.as_ref()?;
        let input = self.ai_check_input();
        let context = dc::review_context(&input, &state.standard(), &state.findings);
        Some(ChatRequest {
            messages: vec![
                ChatMessage::system(dc::review_prompt(&context)),
                ChatMessage::user("Review this drawing."),
            ],
            temperature: Some(0.1),
            json_mode: true,
        })
    }

    fn ai_check_estimate(&mut self) {
        let (settings, keys) = crate::ai::settings::current();
        let estimate = match settings.route(AiFeature::DrawingChecker, &keys) {
            Ok(route) => match self.ai_check.as_ref().filter(|s| s.ran) {
                Some(_) => self
                    .ai_check_review_request()
                    .map(|r| crate::ai::describe_estimate(&route, &r))
                    .unwrap_or_default(),
                None => format!("AI review: {} · {}", route.provider, route.model),
            },
            Err(_) => {
                "Rule checks run locally; set up a model in AISETTINGS for the AI review.".into()
            }
        };
        if let Some(state) = self.ai_check.as_mut() {
            state.estimate = estimate;
        }
    }

    fn ai_check_run(&mut self) {
        let input = self.ai_check_input();
        let Some(state) = self.ai_check.as_mut() else {
            return;
        };
        state.findings = dc::run_rules(&input, &state.standard());
        state.objects_checked = input.items.len();
        state.ran = true;
        state.ai_summary.clear();
        state.confirm_armed = false;
        let (e, w) = state
            .findings
            .iter()
            .fold((0, 0), |(e, w), f| match f.severity {
                dc::Severity::Error => (e + 1, w),
                dc::Severity::Warning => (e, w + 1),
                dc::Severity::Info => (e, w),
            });
        state.status = Some(Ok(format!(
            "Checked {} object(s) against {}: {e} error(s), {w} warning(s), {} note(s).",
            input.items.len(),
            state.standard().name,
            state.findings.len() - e - w
        )));
    }

    pub(in crate::app) fn on_ai_check(&mut self, msg: AiCheckMsg) -> Task<Message> {
        let Some(state) = self.ai_check.as_mut() else {
            return Task::none();
        };
        match msg {
            AiCheckMsg::Standard(kind) => {
                state.standard = kind;
                if state.ran {
                    self.ai_check_run();
                }
            }
            AiCheckMsg::LoadCompany => {
                return Task::perform(pick_company(), |r| {
                    Message::AiCheck(AiCheckMsg::CompanyLoaded(r))
                });
            }
            AiCheckMsg::CompanyLoaded(Ok((path, standard))) => {
                state.status = Some(Ok(format!("Loaded \"{}\" from {path}.", standard.name)));
                state.company = Some(standard);
                state.standard = StandardKind::Company;
                if state.ran {
                    self.ai_check_run();
                }
            }
            AiCheckMsg::CompanyLoaded(Err(e)) => {
                if e != "Cancelled" {
                    state.status = Some(Err(e));
                }
            }
            AiCheckMsg::Run => self.ai_check_run(),
            AiCheckMsg::Review => return self.ai_check_review(),
            AiCheckMsg::ReviewReply(result) => self.ai_check_review_reply(result),
            AiCheckMsg::Show(n) => {
                let Some(f) = state.findings.get(n) else {
                    return Task::none();
                };
                let handles: Vec<codec::Handle> = f
                    .handles
                    .iter()
                    .filter_map(|h| u64::from_str_radix(h, 16).ok())
                    .map(codec::Handle::new)
                    .collect();
                let i = self.active_tab;
                let scene = &mut self.tabs[i].scene;
                scene.deselect_all();
                for h in &handles {
                    scene.select_entity(*h, false);
                }
                scene.zoom_to_entities(&handles);
                self.refresh_selected_grips();
                // Step out of the dialog so the objects can be seen; AICHECK
                // brings the findings back.
                self.active_modal = None;
                self.command_line.push_info(&format!(
                    "AICHECK: {} object(s) selected. Type AICHECK to return to the findings.",
                    handles.len()
                ));
                return Task::none();
            }
            AiCheckMsg::Fix(n) => {
                let Some(f) = state.findings.get(n).cloned() else {
                    return Task::none();
                };
                let plan = Plan {
                    summary: format!("Erase {} object(s) ({})", f.handles.len(), f.rule),
                    message: None,
                    steps: vec![json!({"op": "entities_delete", "handles": f.handles})],
                };
                let outcome = self.apply_ai_plan(&plan);
                self.ai_check_run();
                if let Some(state) = self.ai_check.as_mut() {
                    state.status = Some(match outcome {
                        Ok(_) => Ok(format!(
                            "Erased {} object(s); one UNDO restores them.",
                            f.handles.len()
                        )),
                        Err(e) => Err(e),
                    });
                }
            }
            AiCheckMsg::OpenSettings => {
                self.open_ai_settings();
                return Task::none();
            }
        }
        self.ai_check_estimate();
        Task::none()
    }

    fn ai_check_review(&mut self) -> Task<Message> {
        let (settings, keys) = crate::ai::settings::current();
        let route = match settings.route(AiFeature::DrawingChecker, &keys) {
            Ok(r) => r,
            Err(e) => {
                if let Some(state) = self.ai_check.as_mut() {
                    state.status = Some(Err(format!("{e}. Open Settings to choose a model.")));
                }
                return Task::none();
            }
        };
        let armed = self.ai_check.as_ref().is_some_and(|s| s.confirm_armed);
        if settings.confirm_before_send && !route.is_local && !armed {
            self.ai_check_estimate();
            if let Some(state) = self.ai_check.as_mut() {
                state.confirm_armed = true;
            }
            return Task::none();
        }
        let Some(request) = self.ai_check_review_request() else {
            return Task::none();
        };
        if let Some(state) = self.ai_check.as_mut() {
            state.busy = true;
            state.confirm_armed = false;
            state.status = None;
        }
        Task::perform(
            crate::ai::run_blocking(move || crate::ai::client::chat(&route, &request)),
            |r| Message::AiCheck(AiCheckMsg::ReviewReply(r)),
        )
    }

    fn ai_check_review_reply(&mut self, result: Result<ChatResponse, String>) {
        let input = self.ai_check_input();
        let Some(state) = self.ai_check.as_mut() else {
            return;
        };
        state.busy = false;
        match result.and_then(|r| dc::parse_review(&r.content, &input)) {
            Ok((summary, found)) => {
                state.findings.retain(|f| !f.from_ai);
                let n = found.len();
                state.findings.extend(found);
                state.findings.sort_by(|a, b| {
                    a.severity
                        .cmp(&b.severity)
                        .then_with(|| a.from_ai.cmp(&b.from_ai))
                });
                state.ai_summary = summary;
                state.status = Some(Ok(format!(
                    "The AI review added {n} finding(s). They are suggestions — check them against the drawing."
                )));
            }
            Err(e) => state.status = Some(Err(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::OpenCADStudio;

    #[test]
    fn checks_a_real_drawing_and_erases_duplicates_as_one_step() {
        let mut app = OpenCADStudio::new_for_test();
        app.automation_op(r#"{"op":"new"}"#);
        let i = app.active_tab;
        app.tabs[i].scene.document.header.insertion_units = 4;
        app.automation_op(r#"{"op":"run","cmd":"LINE 0,0 10,0"}"#);
        app.automation_op(r#"{"op":"run","cmd":"LINE 0,0 10,0"}"#);
        app.automation_op(r#"{"op":"run","cmd":"CIRCLE 5,5 2"}"#);
        app.open_ai_check();
        app.on_ai_check(crate::ui::window::ai_check::AiCheckMsg::Run);
        let state = app.ai_check.as_ref().unwrap();
        assert_eq!(state.objects_checked, 3);
        let dup = state
            .findings
            .iter()
            .position(|f| f.rule == "DUPLICATE")
            .expect("duplicate line found");
        assert!(
            state.findings.iter().any(|f| f.rule == "LAYER-0"),
            "{:?}",
            state.findings
        );
        app.on_ai_check(crate::ui::window::ai_check::AiCheckMsg::Fix(dup));
        let state = app.ai_check.as_ref().unwrap();
        assert_eq!(state.objects_checked, 2);
        assert!(!state.findings.iter().any(|f| f.rule == "DUPLICATE"));
        app.active_modal = None;
        app.automation_op(r#"{"op":"undo"}"#);
        assert_eq!(app.automation_op(r#"{"op":"entities"}"#)["total"], 3);
    }
}
