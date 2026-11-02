//! Constraint inference (AICONSTRAIN): collects the sketch primitives of the
//! selection, asks the model for design-intent constraints and a dimension
//! scheme, and applies the ticked ones (plus Auto Constrain's) as one undo
//! step — undone again if the solver had to move anything.

use super::{Message, ModalKind, OpenCADStudio};
use crate::ai::constraint_suggest::{self as cs, Geom, Kind, Prim, RefSpec, Role, Tolerance};
use crate::ai::{AiFeature, ChatMessage, ChatRequest, ChatResponse};
use crate::scene::named_parameters::DrivingValue;
use crate::scene::parametric_constraints::{ConstraintKind, ParametricRef};
use crate::ui::window::ai_constrain::{AiConstrainMsg, State};
use codec::{EntityType, Handle};
use iced::Task;

const MAX_OBJECTS: usize = 200;

/// The sketch primitives of `handles`: lines, circles, arcs and the straight
/// segments of 2D polylines.
fn primitives(doc: &codec::Document, handles: &[Handle]) -> Vec<Prim> {
    let hex = |h: Handle| format!("{:X}", h.value());
    let mut out = Vec::new();
    for h in handles {
        match doc.get_entity(*h) {
            Some(EntityType::Line(l)) => out.push(Prim {
                handle: hex(*h),
                segment: None,
                vertices: 0,
                geom: Geom::Line {
                    a: [l.start.x, l.start.y],
                    b: [l.end.x, l.end.y],
                },
            }),
            Some(EntityType::Circle(c)) => out.push(Prim {
                handle: hex(*h),
                segment: None,
                vertices: 0,
                geom: Geom::Circle {
                    c: [c.center.x, c.center.y],
                    r: c.radius,
                },
            }),
            Some(EntityType::Arc(a)) => {
                let at = |deg: f64| {
                    let t = deg;
                    [
                        a.center.x + a.radius * t.cos(),
                        a.center.y + a.radius * t.sin(),
                    ]
                };
                out.push(Prim {
                    handle: hex(*h),
                    segment: None,
                    vertices: 0,
                    geom: Geom::Arc {
                        c: [a.center.x, a.center.y],
                        r: a.radius,
                        a: at(a.start_angle),
                        b: at(a.end_angle),
                    },
                })
            }
            Some(EntityType::LwPolyline(p)) => {
                let Some(world) = crate::entities::curve::lwpolyline_world_xy(p) else {
                    continue;
                };
                let n = world.vertices.len();
                for k in 0..n {
                    let next = k + 1;
                    if next >= n && !world.is_closed {
                        break;
                    }
                    if world.vertices[k].bulge.abs() > 1e-9 {
                        continue;
                    }
                    let (a, b) = (
                        world.vertices[k].location,
                        world.vertices[next % n].location,
                    );
                    out.push(Prim {
                        handle: hex(*h),
                        segment: Some(k),
                        vertices: n,
                        geom: Geom::Line {
                            a: [a.x, a.y],
                            b: [b.x, b.y],
                        },
                    });
                }
            }
            _ => {}
        }
    }
    out
}

fn handle_of(p: &Prim) -> Handle {
    Handle::new(u64::from_str_radix(&p.handle, 16).unwrap_or(0))
}

/// The scene reference for a role of a primitive.
fn to_ref(prims: &[Prim], r: RefSpec) -> ParametricRef {
    let p = &prims[r.prim];
    let h = handle_of(p);
    match (p.segment, r.role) {
        (Some(k), Role::Whole) => ParametricRef::segment(h, k),
        (Some(k), Role::Start) => ParametricRef::point(h, k as i32),
        (Some(k), Role::End) => ParametricRef::point(h, ((k + 1) % p.vertices.max(1)) as i32),
        (_, Role::Center) => ParametricRef::center(h),
        (None, Role::Whole) => ParametricRef::whole(h),
        (None, Role::Start) => ParametricRef::point(h, 0),
        (None, Role::End) => ParametricRef::point(h, 1),
    }
}

/// The primitive reference for a scene reference, when it is one of ours.
fn from_ref(prims: &[Prim], r: &ParametricRef) -> Option<RefSpec> {
    let hex = format!("{:X}", r.entity.value());
    prims.iter().enumerate().find_map(|(n, p)| {
        if p.handle != hex {
            return None;
        }
        let candidates = [Role::Whole, Role::Start, Role::End, Role::Center];
        candidates
            .into_iter()
            .find(|role| {
                matches!(
                    (&p.geom, role),
                    (Geom::Line { .. }, Role::Whole | Role::Start | Role::End)
                        | (Geom::Circle { .. }, Role::Whole | Role::Center)
                        | (Geom::Arc { .. }, _)
                ) && to_ref(
                    prims,
                    RefSpec {
                        prim: n,
                        role: *role,
                    },
                ) == *r
            })
            .map(|role| RefSpec { prim: n, role })
    })
}

fn kind_of(k: ConstraintKind) -> Option<Kind> {
    Some(match k {
        ConstraintKind::Horizontal => Kind::Horizontal,
        ConstraintKind::Vertical => Kind::Vertical,
        ConstraintKind::Parallel => Kind::Parallel,
        ConstraintKind::Perpendicular => Kind::Perpendicular,
        ConstraintKind::Equal => Kind::Equal,
        ConstraintKind::Concentric => Kind::Concentric,
        ConstraintKind::Coincident => Kind::Coincident,
        ConstraintKind::Tangent => Kind::Tangent,
        ConstraintKind::Symmetric => Kind::Symmetric,
        ConstraintKind::Fixed => Kind::Fixed,
        ConstraintKind::Distance => Kind::Distance,
        ConstraintKind::DistanceX => Kind::DistanceX,
        ConstraintKind::DistanceY => Kind::DistanceY,
        ConstraintKind::Radius => Kind::Radius,
        ConstraintKind::Diameter => Kind::Diameter,
        _ => return None,
    })
}

/// The scene constraint for a checked suggestion.
fn to_scene(
    s: &cs::Suggestion,
    prims: &[Prim],
) -> (ConstraintKind, Vec<ParametricRef>, Option<DrivingValue>) {
    let kind = match s.kind {
        Kind::Horizontal => ConstraintKind::Horizontal,
        Kind::Vertical => ConstraintKind::Vertical,
        Kind::Parallel => ConstraintKind::Parallel,
        Kind::Perpendicular => ConstraintKind::Perpendicular,
        Kind::Equal => ConstraintKind::Equal,
        Kind::Concentric => ConstraintKind::Concentric,
        Kind::Coincident => ConstraintKind::Coincident,
        Kind::Tangent => ConstraintKind::Tangent,
        Kind::Symmetric => ConstraintKind::Symmetric,
        Kind::Fixed => ConstraintKind::Fixed,
        Kind::Distance => ConstraintKind::Distance,
        Kind::DistanceX => ConstraintKind::DistanceX,
        Kind::DistanceY => ConstraintKind::DistanceY,
        Kind::Radius => ConstraintKind::Radius,
        Kind::Diameter => ConstraintKind::Diameter,
    };
    let refs: Vec<ParametricRef> = match (s.kind, s.refs.as_slice()) {
        // A line's length is the distance between its two ends.
        (Kind::Distance, [one]) => vec![
            to_ref(
                prims,
                RefSpec {
                    prim: one.prim,
                    role: Role::Start,
                },
            ),
            to_ref(
                prims,
                RefSpec {
                    prim: one.prim,
                    role: Role::End,
                },
            ),
        ],
        // Concentric relates the two centres.
        (Kind::Concentric, [a, b]) => vec![
            to_ref(
                prims,
                RefSpec {
                    prim: a.prim,
                    role: Role::Center,
                },
            ),
            to_ref(
                prims,
                RefSpec {
                    prim: b.prim,
                    role: Role::Center,
                },
            ),
        ],
        (_, refs) => refs.iter().map(|r| to_ref(prims, *r)).collect(),
    };
    (kind, refs, s.value.map(DrivingValue::Literal))
}

fn max_shift(before: &[Prim], after: &[Prim]) -> f64 {
    let pts = |p: &Prim| -> Vec<[f64; 2]> {
        match &p.geom {
            Geom::Line { a, b } => vec![*a, *b],
            Geom::Circle { c, r } => vec![*c, [*r, 0.0]],
            Geom::Arc { c, r, a, b } => vec![*c, [*r, 0.0], *a, *b],
        }
    };
    if before.len() != after.len() {
        return f64::INFINITY;
    }
    before
        .iter()
        .zip(after)
        .flat_map(|(x, y)| {
            pts(x)
                .into_iter()
                .zip(pts(y))
                .map(|(p, q)| (p[0] - q[0]).hypot(p[1] - q[1]))
                .collect::<Vec<_>>()
        })
        .fold(0.0, f64::max)
}

impl OpenCADStudio {
    fn ai_constrain_tolerance(&self) -> Tolerance {
        Tolerance {
            distance: self.auto_constrain_settings.distance_tolerance.max(1e-9),
            angle_rad: self
                .auto_constrain_settings
                .angle_tolerance_deg
                .max(1e-6)
                .to_radians(),
        }
    }

    pub(in crate::app) fn open_ai_constrain(&mut self) {
        let i = self.active_tab;
        if self.tabs[i].is_start {
            self.command_line
                .push_error("Open a drawing before using AICONSTRAIN.");
            return;
        }
        let scene = &self.tabs[i].scene;
        let mut handles = scene.selected_handles_in_order();
        let whole_space = handles.is_empty();
        if whole_space {
            let model = scene.document.header.model_space_block_handle;
            handles = scene
                .document
                .entities()
                .filter(|e| e.common().owner_handle == model)
                .filter(|e| {
                    matches!(
                        e,
                        EntityType::Line(_)
                            | EntityType::Circle(_)
                            | EntityType::Arc(_)
                            | EntityType::LwPolyline(_)
                    )
                })
                .map(|e| e.common().handle)
                .collect();
        }
        if handles.len() > MAX_OBJECTS {
            self.command_line.push_error(&format!(
                "AICONSTRAIN works on up to {MAX_OBJECTS} objects; select one sketch ({} found).",
                handles.len()
            ));
            return;
        }
        let prims = primitives(&scene.document, &handles);
        if prims.is_empty() {
            self.command_line
                .push_error("AICONSTRAIN: select lines, arcs, circles or polylines to constrain.");
            return;
        }
        let scope = self.tabs[i].current_parametric_scope();
        let auto =
            scene.inferred_parametric_constraints(scope, &handles, &self.auto_constrain_settings);
        self.ai_constrain = Some(State {
            handles,
            prims,
            auto,
            include_auto: true,
            intent: String::new(),
            suggestions: Vec::new(),
            accepted: Vec::new(),
            rejected: Vec::new(),
            busy: false,
            status: whole_space.then(|| {
                Ok(
                    "Nothing was selected, so every sketch object in model space is included."
                        .to_string(),
                )
            }),
            estimate: String::new(),
            confirm_armed: false,
        });
        self.active_modal = Some(ModalKind::AiConstrain);
        self.reset_modal_geometry();
        self.ai_constrain_estimate();
    }

    /// Existing relations (Auto Constrain's and those already in the
    /// drawing) in primitive terms, for the prompt and de-duplication.
    fn ai_constrain_existing(&self, state: &State) -> Vec<(Kind, Vec<RefSpec>)> {
        let i = self.active_tab;
        let scope = self.tabs[i].current_parametric_scope();
        let present = self.tabs[i]
            .scene
            .parametric_constraint_set(scope)
            .map(|set| {
                set.constraints
                    .iter()
                    .map(|c| (c.kind, c.refs.clone()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        state
            .auto
            .iter()
            .cloned()
            .chain(present)
            .filter_map(|(k, refs)| {
                let kind = kind_of(k)?;
                let specs: Option<Vec<RefSpec>> =
                    refs.iter().map(|r| from_ref(&state.prims, r)).collect();
                Some((kind, specs?))
            })
            .collect()
    }

    fn ai_constrain_request(&self) -> Option<ChatRequest> {
        let state = self.ai_constrain.as_ref()?;
        let existing = self.ai_constrain_existing(state);
        let units = self.ai_drawing_units().unwrap_or("drawing units");
        Some(ChatRequest {
            messages: vec![
                ChatMessage::system(cs::prompt(&state.prims, &existing, units)),
                ChatMessage::user("Suggest the constraints."),
            ],
            temperature: Some(0.1),
            json_mode: true,
        })
    }

    fn ai_constrain_estimate(&mut self) {
        let (settings, keys) = crate::ai::settings::current();
        let estimate = match settings.route(AiFeature::ConstraintInference, &keys) {
            Ok(route) => self
                .ai_constrain_request()
                .map(|r| crate::ai::describe_estimate(&route, &r))
                .unwrap_or_default(),
            Err(_) => {
                "Auto Constrain works without a model; set one up in AISETTINGS for AI suggestions."
                    .into()
            }
        };
        if let Some(state) = self.ai_constrain.as_mut() {
            state.estimate = estimate;
        }
    }

    pub(in crate::app) fn on_ai_constrain(&mut self, msg: AiConstrainMsg) -> Task<Message> {
        let Some(state) = self.ai_constrain.as_mut() else {
            return Task::none();
        };
        match msg {
            AiConstrainMsg::Toggle(n, v) => {
                if let Some(a) = state.accepted.get_mut(n) {
                    *a = v;
                }
            }
            AiConstrainMsg::IncludeAuto(v) => state.include_auto = v,
            AiConstrainMsg::All(v) => state.accepted.iter_mut().for_each(|a| *a = v),
            AiConstrainMsg::OpenSettings => self.open_ai_settings(),
            AiConstrainMsg::Analyze => return self.ai_constrain_analyze(),
            AiConstrainMsg::Reply(result) => self.ai_constrain_reply(result),
            AiConstrainMsg::Apply => {
                let outcome = self.ai_constrain_apply();
                match outcome {
                    Ok(report) => {
                        self.command_line
                            .push_info(&format!("AICONSTRAIN: {report}"));
                        self.ai_constrain = None;
                        self.active_modal = None;
                    }
                    Err(e) => {
                        if let Some(state) = self.ai_constrain.as_mut() {
                            state.status = Some(Err(e));
                        }
                    }
                }
            }
        }
        Task::none()
    }

    fn ai_constrain_analyze(&mut self) -> Task<Message> {
        let (settings, keys) = crate::ai::settings::current();
        let route = match settings.route(AiFeature::ConstraintInference, &keys) {
            Ok(r) => r,
            Err(e) => {
                if let Some(state) = self.ai_constrain.as_mut() {
                    state.status = Some(Err(format!("{e}. Open Settings to choose a model.")));
                }
                return Task::none();
            }
        };
        let armed = self.ai_constrain.as_ref().is_some_and(|s| s.confirm_armed);
        if settings.confirm_before_send && !route.is_local && !armed {
            self.ai_constrain_estimate();
            if let Some(state) = self.ai_constrain.as_mut() {
                state.confirm_armed = true;
            }
            return Task::none();
        }
        let Some(request) = self.ai_constrain_request() else {
            return Task::none();
        };
        if let Some(state) = self.ai_constrain.as_mut() {
            state.busy = true;
            state.confirm_armed = false;
            state.status = None;
        }
        Task::perform(
            crate::ai::run_blocking(move || crate::ai::client::chat(&route, &request)),
            |r| Message::AiConstrain(AiConstrainMsg::Reply(r)),
        )
    }

    fn ai_constrain_reply(&mut self, result: Result<ChatResponse, String>) {
        let tol = self.ai_constrain_tolerance();
        let existing = match self.ai_constrain.as_ref() {
            Some(state) => self.ai_constrain_existing(state),
            None => return,
        };
        let Some(state) = self.ai_constrain.as_mut() else {
            return;
        };
        state.busy = false;
        match result.and_then(|r| cs::parse_suggestions(&r.content, &state.prims, tol, &existing)) {
            Ok((intent, suggestions, rejected)) => {
                state.status = Some(Ok(format!(
                    "{} suggestion(s) hold for the current geometry{}.",
                    suggestions.len(),
                    if rejected.is_empty() {
                        String::new()
                    } else {
                        format!("; {} dropped", rejected.len())
                    }
                )));
                state.intent = intent;
                state.accepted = vec![true; suggestions.len()];
                state.suggestions = suggestions;
                state.rejected = rejected;
            }
            Err(e) => state.status = Some(Err(e)),
        }
    }

    /// Add the chosen constraints as one undo step; if solving them moved
    /// any geometry, take the step back and say so.
    pub(in crate::app) fn ai_constrain_apply(&mut self) -> Result<String, String> {
        let i = self.active_tab;
        let Some(state) = self.ai_constrain.as_ref() else {
            return Err("nothing to apply".into());
        };
        let mut chosen: Vec<(ConstraintKind, Vec<ParametricRef>, Option<DrivingValue>)> =
            Vec::new();
        if state.include_auto {
            chosen.extend(state.auto.iter().map(|(k, r)| (*k, r.clone(), None)));
        }
        for (s, on) in state.suggestions.iter().zip(&state.accepted) {
            if *on {
                chosen.push(to_scene(s, &state.prims));
            }
        }
        if chosen.is_empty() {
            return Err("no constraints are ticked".into());
        }
        let handles = state.handles.clone();
        let before_prims = state.prims.clone();
        let tol = self.ai_constrain_tolerance();

        let scope = self.tabs[i].current_parametric_scope();
        let before = self.tabs[i]
            .scene
            .parametric_constraint_set(scope)
            .cloned()
            .unwrap_or_else(|| {
                crate::scene::parametric_constraints::ParametricConstraintSet::new(scope)
            });
        self.finish_pending_history(i);
        let undo_before = self.tabs[i].history.undo_stack.len();
        let pending = self.begin_undo(i, "AICONSTRAIN", handles.len(), true);
        self.tabs[i]
            .scene
            .record_undo_parametric_constraints_before(scope, before);
        let count = chosen.len();
        for (kind, refs, value) in chosen {
            let id = self.tabs[i]
                .scene
                .parametric_constraint_set_mut(scope)
                .add(kind, refs, value);
            self.tabs[i].scene.note_parametric_constraint_applied(
                scope,
                id,
                self.constraint_bar_display,
            );
        }
        let changes: Vec<_> = handles
            .iter()
            .map(|h| (*h, crate::scene::ChangeKind::Modified))
            .collect();
        self.tabs[i].scene.bump_entities_with_parametric_policy(
            &changes,
            &[],
            self.constraint_solve_mode,
        );
        self.tabs[i].dirty = true;
        if let Some(pd) = pending {
            self.commit_undo_delta(i, pd);
        }
        self.refresh_properties();

        let after = primitives(&self.tabs[i].scene.document, &handles);
        let shift = max_shift(&before_prims, &after);
        if shift > tol.distance.max(1e-6) {
            if self.tabs[i].history.undo_stack.len() > undo_before {
                self.undo_steps_reported(1, false);
            }
            return Err(format!(
                "these constraints conflict — solving them moved geometry by {shift:.4}, so nothing was applied. Untick some and try again."
            ));
        }
        Ok(format!(
            "{count} constraint(s) applied to {} object(s); one UNDO removes them.",
            handles.len()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 100 × 60 rectangle of lines with two Ø10 holes.
    fn sketch(app: &mut OpenCADStudio) -> Vec<Handle> {
        app.automation_op(r#"{"op":"new"}"#);
        for cmd in [
            "LINE 0,0 100,0",
            "LINE 100,0 100,60",
            "LINE 100,60 0,60",
            "LINE 0,60 0,0",
            "CIRCLE 20,30 5",
            "CIRCLE 80,30 5",
        ] {
            app.automation_op(&serde_json::json!({"op":"run","cmd":cmd}).to_string());
        }
        let i = app.active_tab;
        app.tabs[i]
            .scene
            .document
            .entities()
            .map(|e| e.common().handle)
            .collect()
    }

    #[test]
    fn applies_checked_suggestions_without_moving_geometry_as_one_step() {
        let mut app = OpenCADStudio::new_for_test();
        let handles = sketch(&mut app);
        assert_eq!(handles.len(), 6);
        app.open_ai_constrain();
        let state = app.ai_constrain.as_ref().expect("dialog opened");
        assert_eq!(state.prims.len(), 6);
        assert!(!state.auto.is_empty(), "auto constrain found relations");
        let tol = app.ai_constrain_tolerance();
        let existing = app.ai_constrain_existing(state);
        let reply = r#"{"intent":"plate","constraints":[
            {"kind":"equal","refs":[4,5]},
            {"kind":"diameter","refs":[4]},
            {"kind":"distance","refs":[0]},
            {"kind":"distance","refs":[1]},
            {"kind":"distance_x","refs":["0.start","4.center"]},
            {"kind":"distance_y","refs":["0.start","4.center"]},
            {"kind":"distance_x","refs":["4.center","5.center"]}
        ]}"#;
        let (_, ok, bad) = cs::parse_suggestions(reply, &state.prims, tol, &existing).unwrap();
        assert!(bad.is_empty(), "{bad:?}");
        let n = ok.len();
        let st = app.ai_constrain.as_mut().unwrap();
        st.accepted = vec![true; n];
        st.suggestions = ok;
        let scope = app.tabs[app.active_tab].current_parametric_scope();
        let count_before = app.tabs[app.active_tab]
            .scene
            .parametric_constraint_set(scope)
            .map_or(0, |s| s.constraints.len());
        let report = app.ai_constrain_apply().expect("applies");
        assert!(report.contains("constraint"), "{report}");
        let count_after = app.tabs[app.active_tab]
            .scene
            .parametric_constraint_set(scope)
            .map_or(0, |s| s.constraints.len());
        assert!(
            count_after > count_before + n - 1,
            "{count_before} → {count_after}"
        );
        // Undo removes them all.
        app.active_modal = None;
        app.automation_op(r#"{"op":"undo"}"#);
        let count_undone = app.tabs[app.active_tab]
            .scene
            .parametric_constraint_set(scope)
            .map_or(0, |s| s.constraints.len());
        assert_eq!(count_undone, count_before);
    }

    #[test]
    fn refs_round_trip_between_scene_and_primitives() {
        let mut app = OpenCADStudio::new_for_test();
        sketch(&mut app);
        app.open_ai_constrain();
        let state = app.ai_constrain.as_ref().unwrap();
        for (n, p) in state.prims.iter().enumerate() {
            let roles: &[Role] = match p.geom {
                Geom::Line { .. } => &[Role::Whole, Role::Start, Role::End],
                _ => &[Role::Whole, Role::Center],
            };
            for role in roles {
                let spec = RefSpec {
                    prim: n,
                    role: *role,
                };
                assert_eq!(
                    from_ref(&state.prims, &to_ref(&state.prims, spec)),
                    Some(spec)
                );
            }
        }
    }

    #[test]
    fn a_constraint_that_would_move_geometry_is_taken_back() {
        let mut app = OpenCADStudio::new_for_test();
        sketch(&mut app);
        app.open_ai_constrain();
        let scope = app.tabs[app.active_tab].current_parametric_scope();
        let count_before = app.tabs[app.active_tab]
            .scene
            .parametric_constraint_set(scope)
            .map_or(0, |s| s.constraints.len());
        let st = app.ai_constrain.as_mut().unwrap();
        st.include_auto = false;
        // Bypass the checks: a length the line does not have.
        st.suggestions = vec![cs::Suggestion {
            kind: Kind::Distance,
            refs: vec![RefSpec {
                prim: 0,
                role: Role::Whole,
            }],
            value: Some(90.0),
            why: String::new(),
        }];
        st.accepted = vec![true];
        let before = st.prims.clone();
        let err = app.ai_constrain_apply().unwrap_err();
        assert!(err.contains("conflict"), "{err}");
        let i = app.active_tab;
        let handles = app.ai_constrain.as_ref().unwrap().handles.clone();
        let after = primitives(&app.tabs[i].scene.document, &handles);
        assert!(max_shift(&before, &after) < 1e-9, "geometry restored");
        let count_after = app.tabs[i]
            .scene
            .parametric_constraint_set(scope)
            .map_or(0, |s| s.constraints.len());
        assert_eq!(count_after, count_before);
    }
}
