//! Natural-language command bar (AICMD): the prompt sent to the model and
//! the validation of the plan it returns.
//!
//! The model never touches the drawing. It answers with a JSON plan of
//! automation-API operations (the same ops `--serve`, REST and MCP clients
//! use). The plan is validated here, shown to the person step by step, and
//! only applied when they press Apply — as a single undo step.

use serde_json::{json, Value};

/// Operations a plan may contain. Everything else (save, open, close,
/// plugin loading, file export, …) is refused before the plan is shown.
pub const ALLOWED_OPS: &[&str] = &[
    "entities_create",
    "entities_transform",
    "entities_delete",
    "run",
    "select",
];

/// Command verbs a `run` step may not start: they touch files, sessions,
/// other programs or the AI tools themselves.
const DENIED_VERBS: &[&str] = &[
    "OPEN",
    "NEW",
    "QNEW",
    "CLOSE",
    "CLOSEALL",
    "QUIT",
    "EXIT",
    "SAVE",
    "SAVEAS",
    "SAVEALL",
    "QSAVE",
    "WBLOCK",
    "EXPORT",
    "EXPORTSTL",
    "EXPORTSTEP",
    "EXPORTPDF",
    "PLOT",
    "PUBLISH",
    "PRINTALL",
    "SCRIPT",
    "SCR",
    "SHELL",
    "SH",
    "RECOVER",
    "AUDIT",
    "PURGE",
    "-PURGE",
    "ETRANSMIT",
    "ATTACH",
    "XATTACH",
    "IMAGEATTACH",
    "PDFATTACH",
    "INSERTOBJ",
    "OLE",
    "PLUGINS",
    "APPLOAD",
    "NETLOAD",
    "HYPERLINK",
    "BROWSER",
    "DONATE",
    "WEBVERSION",
    "UNDO",
    "U",
    "REDO",
    "MREDO",
    "AICMD",
    "AICHAT",
    "AISETTINGS",
    "AIOPTIONS",
];

pub const MAX_STEPS: usize = 40;

/// A validated plan, ready to show and apply.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub summary: String,
    /// Shown instead of steps when the model needs more information.
    pub message: Option<String>,
    pub steps: Vec<Value>,
}

fn verb_of(cmd: &str) -> String {
    cmd.trim()
        .trim_start_matches(['_', '.', '\''])
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_uppercase()
}

/// Parse and check a model reply. Errors name the offending step.
pub fn parse_plan(reply: &str) -> Result<Plan, String> {
    let value = super::client::extract_json(reply)
        .ok_or_else(|| "the model did not answer with a JSON plan".to_string())?;
    let obj = match &value {
        Value::Object(_) => value.clone(),
        Value::Array(steps) => json!({ "summary": "", "steps": steps }),
        _ => return Err("the model's plan is not a JSON object".into()),
    };
    let summary = obj["summary"].as_str().unwrap_or("").trim().to_string();
    let message = obj["message"]
        .as_str()
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .map(str::to_string);
    let steps: Vec<Value> = obj["steps"].as_array().cloned().unwrap_or_default();
    if steps.len() > MAX_STEPS {
        return Err(format!(
            "the plan has {} steps; at most {MAX_STEPS} are allowed — ask for a smaller change",
            steps.len()
        ));
    }
    for (n, step) in steps.iter().enumerate() {
        validate_step(step).map_err(|e| format!("step {}: {e}", n + 1))?;
    }
    if steps.is_empty() && message.is_none() {
        return Err("the model returned an empty plan".into());
    }
    Ok(Plan {
        summary,
        message,
        steps,
    })
}

fn validate_step(step: &Value) -> Result<(), String> {
    let obj = step.as_object().ok_or("not a JSON object")?;
    let op = obj
        .get("op")
        .and_then(Value::as_str)
        .ok_or("missing \"op\"")?;
    if !ALLOWED_OPS.contains(&op) {
        return Err(format!("operation \"{op}\" is not allowed in an AI plan"));
    }
    for reserved in ["protocol", "request_id", "document_id"] {
        if obj.contains_key(reserved) {
            return Err(format!("\"{reserved}\" is filled in by Open CAD Studio"));
        }
    }
    match op {
        "entities_create" => {
            let entities = obj
                .get("entities")
                .and_then(Value::as_array)
                .ok_or("entities_create needs an \"entities\" array")?;
            if entities.is_empty() || entities.len() > 2000 {
                return Err("entities_create needs 1–2000 entities".into());
            }
            if entities
                .iter()
                .any(|e| e.get("type").and_then(Value::as_str).is_none())
            {
                return Err("every entity needs a \"type\"".into());
            }
        }
        "entities_transform" => {
            if obj.get("action").and_then(Value::as_str).is_none() {
                return Err("entities_transform needs an \"action\"".into());
            }
            check_handles(obj.get("handles"))?;
        }
        "entities_delete" => check_handles(obj.get("handles"))?,
        "run" => {
            let cmd = obj
                .get("cmd")
                .and_then(Value::as_str)
                .ok_or("run needs a \"cmd\" string")?;
            let verb = verb_of(cmd);
            if verb.is_empty() {
                return Err("run has an empty command".into());
            }
            if DENIED_VERBS.contains(&verb.as_str()) {
                return Err(format!("the {verb} command is not allowed in an AI plan"));
            }
            if cmd.contains('\n') {
                return Err("run takes one command line".into());
            }
        }
        _ => {}
    }
    Ok(())
}

fn check_handles(v: Option<&Value>) -> Result<(), String> {
    match v {
        Some(Value::String(s)) if is_placeholder(s) => Ok(()),
        Some(Value::Array(a)) if !a.is_empty() && a.iter().all(Value::is_string) => Ok(()),
        _ => Err(
            "\"handles\" must be a list of handles, \"$last\", \"$created\" or \"$selection\""
                .into(),
        ),
    }
}

fn is_placeholder(s: &str) -> bool {
    matches!(s, "$last" | "$created" | "$selection")
}

/// Handles a running plan can refer to by placeholder.
#[derive(Debug, Clone, Default)]
pub struct Bindings {
    pub last: Vec<String>,
    pub created: Vec<String>,
    pub selection: Vec<String>,
}

impl Bindings {
    /// Record what a finished step created.
    pub fn absorb(&mut self, result: &Value) {
        let mut new: Vec<String> = Vec::new();
        for key in ["/result/handles", "/result/created", "/handles", "/created"] {
            if let Some(list) = result.pointer(key).and_then(Value::as_array) {
                new.extend(list.iter().filter_map(Value::as_str).map(str::to_string));
            }
        }
        // A `run` step reports what its command added in `changes`.
        if new.is_empty() {
            if let Some(changes) = result.get("changes").and_then(Value::as_array) {
                new.extend(
                    changes
                        .iter()
                        .filter(|c| c["kind"] == "Added")
                        .filter_map(|c| c["handle"].as_str())
                        .map(str::to_string),
                );
            }
        }
        if !new.is_empty() {
            self.created.extend(new.iter().cloned());
            self.last = new;
        }
    }
}

/// Replace `"$last"` / `"$created"` / `"$selection"` handle placeholders.
pub fn bind(step: &Value, bindings: &Bindings) -> Result<Value, String> {
    let mut step = step.clone();
    if let Some(Value::String(s)) = step.get("handles") {
        let handles = match s.as_str() {
            "$last" => &bindings.last,
            "$created" => &bindings.created,
            "$selection" => &bindings.selection,
            _ => return Ok(step),
        };
        if handles.is_empty() {
            return Err(format!(
                "{s} refers to no objects at this point of the plan"
            ));
        }
        step["handles"] = json!(handles);
    }
    Ok(step)
}

/// One line describing a step for the review list.
pub fn describe_step(step: &Value) -> String {
    let note = step
        .get("note")
        .and_then(Value::as_str)
        .map(|n| format!(" — {n}"))
        .unwrap_or_default();
    let handles = |v: &Value| match v.get("handles") {
        Some(Value::String(s)) => match s.as_str() {
            "$last" => "the objects from the previous step".to_string(),
            "$created" => "everything this plan created".to_string(),
            "$selection" => "the selected objects".to_string(),
            other => other.to_string(),
        },
        Some(Value::Array(a)) => format!("{} object(s)", a.len()),
        _ => "objects".to_string(),
    };
    let text = match step["op"].as_str().unwrap_or("") {
        "entities_create" => {
            let entities = step["entities"].as_array().cloned().unwrap_or_default();
            let mut counts: std::collections::BTreeMap<String, usize> = Default::default();
            let mut layers: std::collections::BTreeSet<String> = Default::default();
            for e in &entities {
                *counts
                    .entry(e["type"].as_str().unwrap_or("?").to_string())
                    .or_default() += 1;
                if let Some(l) = e["layer"].as_str() {
                    layers.insert(l.to_string());
                }
            }
            let kinds = counts
                .iter()
                .map(|(k, n)| format!("{n} {k}"))
                .collect::<Vec<_>>()
                .join(", ");
            if layers.is_empty() {
                format!("Create {kinds}")
            } else {
                format!(
                    "Create {kinds} on layer {}",
                    layers.into_iter().collect::<Vec<_>>().join(", ")
                )
            }
        }
        "entities_transform" => {
            let action = step["action"].as_str().unwrap_or("transform");
            let detail = match action {
                "move" | "copy" => step.get("vector").map(|v| format!(" by {v}")),
                "rotate" => step
                    .get("angle_deg")
                    .map(|a| format!(" {a}° about {}", step.get("center").unwrap_or(&json!("?")))),
                "scale" => step.get("factor").map(|f| format!(" ×{f}")),
                "array" => Some(format!(
                    " {}×{} at {} / {}",
                    step.get("rows").unwrap_or(&json!(1)),
                    step.get("columns").unwrap_or(&json!(1)),
                    step.get("row_spacing").unwrap_or(&json!(0)),
                    step.get("column_spacing").unwrap_or(&json!(0)),
                )),
                _ => None,
            }
            .unwrap_or_default();
            let mut verb = action.to_string();
            if let Some(first) = verb.get_mut(0..1) {
                first.make_ascii_uppercase();
            }
            format!("{verb} {}{detail}", handles(step))
        }
        "entities_delete" => format!("Erase {}", handles(step)),
        "run" => format!("Run: {}", step["cmd"].as_str().unwrap_or("")),
        "select" => "Change the selection".to_string(),
        other => other.to_string(),
    };
    format!("{text}{note}")
}

/// The system prompt: the plan format, the allowed operations and the
/// drawing's context (units, layers, counts, selection).
pub fn system_prompt(context: &Value) -> String {
    format!(
        r#"You are the command assistant inside Open CAD Studio, a 2D/3D CAD program compatible with DWG/DXF.
Turn the user's request into a plan of automation operations. Answer with ONE JSON object and nothing else:

{{"summary": "<one sentence>", "steps": [ <operation>, ... ]}}

If the request is unclear or impossible, answer {{"summary": "", "message": "<question or explanation>", "steps": []}}.

Each step is an object with "op" and its fields, plus an optional "note" (short explanation). Allowed operations:

1. entities_create — {{"op":"entities_create","entities":[...]}}. Every entity may have "layer" (created if missing) and "color" (ACI 1-255, 256 = ByLayer). Types and fields:
   Line: start, end | Circle: center, radius | Arc: center, radius, start_angle_deg, end_angle_deg
   LwPolyline: vertices (>= 2), closed, optional constant_width | Point: location
   Spline: fit_points (>= 2) or control_points, optional degree
   Text: value, position, optional height, rotation_deg | MText: value, position, optional height, width
   Insert: block (must exist), position, optional scale, rotation_deg
   Hatch: boundary (>= 3 points), solid: true or pattern (e.g. "ANSI31"), pattern_scale
   Points are [x, y] or [x, y, z] in world coordinates. Angles are degrees, counterclockwise.
2. entities_transform — {{"op":"entities_transform","handles":..., "action": "move"|"copy" (vector:[dx,dy]) | "rotate" (center, angle_deg) | "scale" (center, factor) | "mirror" (axis:[[x1,y1],[x2,y2]], optional copy:true) | "array" (rows, columns, row_spacing, column_spacing)}}
3. entities_delete — {{"op":"entities_delete","handles":...}}
4. run — {{"op":"run","cmd":"<one command line>"}} for anything the above cannot do (FILLET, CHAMFER, OFFSET, DIMLINEAR, LAYER, CLAYER, ZOOM E, …). The command name is followed by its prompt answers separated by spaces; points are x,y; options by their keyword. Prefer entities_create and entities_transform over run.
5. select — {{"op":"select","handles":[...]}} or {{"op":"select","clear":true}}

"handles" is a list of hex handle strings, or one of these placeholders: "$last" (objects created by the previous step), "$created" (everything created so far in this plan), "$selection" (the objects the user had selected).

Rules: never save, open, close, plot or export files; at most {max} steps; build fillets, arrays and offsets with real geometry or run steps rather than approximations; use the drawing's units; keep new geometry near existing geometry unless told otherwise; put geometry on a sensibly named layer only when the user asks for layers.

Current drawing context (JSON):
{context}"#,
        max = MAX_STEPS,
        context = serde_json::to_string_pretty(context).unwrap_or_default()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_a_simple_plan_in_a_fence() {
        let reply = "```json\n{\"summary\":\"two circles\",\"steps\":[{\"op\":\"entities_create\",\"entities\":[{\"type\":\"Circle\",\"center\":[0,0],\"radius\":5}]},{\"op\":\"entities_transform\",\"handles\":\"$last\",\"action\":\"copy\",\"vector\":[20,0]}]}\n```";
        let plan = parse_plan(reply).unwrap();
        assert_eq!(plan.steps.len(), 2);
        assert_eq!(describe_step(&plan.steps[0]), "Create 1 Circle");
        assert!(
            describe_step(&plan.steps[1]).starts_with("Copy the objects from the previous step by")
        );
    }

    #[test]
    fn refuses_file_and_session_commands() {
        for cmd in [
            "SAVE",
            "_qsave",
            "QUIT",
            "SCRIPT x.scr",
            ".OPEN a.dwg",
            "AICMD hi",
        ] {
            let reply =
                format!("{{\"summary\":\"\",\"steps\":[{{\"op\":\"run\",\"cmd\":\"{cmd}\"}}]}}");
            assert!(parse_plan(&reply).is_err(), "{cmd} should be refused");
        }
        let save = r#"{"summary":"","steps":[{"op":"save","path":"x.dwg"}]}"#;
        assert!(parse_plan(save).unwrap_err().contains("not allowed"));
    }

    #[test]
    fn refuses_envelope_fields_and_bad_handles() {
        let r = r#"{"steps":[{"op":"entities_delete","handles":"everything"}]}"#;
        assert!(parse_plan(r).is_err());
        let r = r#"{"steps":[{"op":"run","cmd":"LINE 0,0 1,1","document_id":3}]}"#;
        assert!(parse_plan(r).is_err());
    }

    #[test]
    fn a_question_is_a_valid_answer() {
        let plan = parse_plan(r#"{"summary":"","message":"How big?","steps":[]}"#).unwrap();
        assert_eq!(plan.message.as_deref(), Some("How big?"));
    }

    #[test]
    fn binds_placeholders_from_results() {
        let mut b = Bindings::default();
        b.absorb(&json!({"ok":true,"result":{"handles":["2A","2B"]}}));
        b.absorb(&json!({"ok":true,"result":{"created":["2C"]}}));
        let step = json!({"op":"entities_delete","handles":"$created"});
        assert_eq!(
            bind(&step, &b).unwrap()["handles"],
            json!(["2A", "2B", "2C"])
        );
        let step = json!({"op":"entities_delete","handles":"$last"});
        assert_eq!(bind(&step, &b).unwrap()["handles"], json!(["2C"]));
        let step = json!({"op":"entities_delete","handles":"$selection"});
        assert!(bind(&step, &b).is_err());
        b.absorb(&json!({"ok":true,"result":{},"changes":[{"handle":"30","kind":"Added"},{"handle":"2A","kind":"Modified"}]}));
        assert_eq!(b.last, vec!["30".to_string()]);
    }
}
