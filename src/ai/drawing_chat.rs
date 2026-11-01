//! Chat with your drawing (AICHAT): the model answers questions by calling
//! read-only automation queries against the open document, so answers come
//! from the drawing database rather than guesses.
//!
//! Each turn the model replies with either a tool call
//! `{"tool": "<op>", "args": {...}}` or a final answer
//! `{"answer": "...", "plan": {...optional AICMD plan...}}`. Tool calls are
//! limited to the read-only operations in [`READ_ONLY_TOOLS`]; a suggested
//! change comes back as a plan the person reviews and applies themselves.

use serde_json::{json, Value};

/// Queries the model may run. None of them change the drawing.
pub const READ_ONLY_TOOLS: &[&str] = &[
    "entities",
    "layers",
    "header",
    "query",
    "measure",
    "audit",
    "text_search",
    "blocks",
    "get_selection",
];

/// Tool calls allowed per question before the model must answer.
pub const MAX_TOOL_CALLS: usize = 8;

/// Tool results longer than this are cut (the model is told).
pub const MAX_TOOL_RESULT_CHARS: usize = 12_000;

#[derive(Debug, Clone, PartialEq)]
pub enum Turn {
    /// Run this read-only op (args already include `"op"`).
    Tool { op: String, request: Value },
    Answer {
        text: String,
        plan: Option<super::command_plan::Plan>,
    },
}

/// Interpret a model reply. Plain prose (no JSON) is taken as the answer.
pub fn parse_turn(reply: &str) -> Result<Turn, String> {
    let Some(value) = super::client::extract_json(reply) else {
        let text = reply.trim();
        if text.is_empty() {
            return Err("the model returned an empty reply".into());
        }
        return Ok(Turn::Answer {
            text: text.to_string(),
            plan: None,
        });
    };
    if let Some(tool) = value.get("tool").and_then(Value::as_str) {
        if !READ_ONLY_TOOLS.contains(&tool) {
            return Err(format!(
                "the model asked for \"{tool}\", which is not a read-only query"
            ));
        }
        let mut request = match value.get("args") {
            Some(Value::Object(m)) => Value::Object(m.clone()),
            None | Some(Value::Null) => json!({}),
            Some(_) => return Err("tool \"args\" must be an object".into()),
        };
        for reserved in ["protocol", "request_id", "document_id", "session_id"] {
            if let Some(m) = request.as_object_mut() {
                m.remove(reserved);
            }
        }
        request["op"] = json!(tool);
        // Keep the answer small: a query defaults to summaries.
        if tool == "query" && request.get("limit").is_none() {
            request["limit"] = json!(200);
        }
        return Ok(Turn::Tool {
            op: tool.to_string(),
            request,
        });
    }
    let text = value
        .get("answer")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("")
        .to_string();
    let plan = match value.get("plan") {
        Some(p)
            if p.get("steps")
                .and_then(Value::as_array)
                .is_some_and(|s| !s.is_empty()) =>
        {
            Some(super::command_plan::parse_plan(&p.to_string())?)
        }
        _ => None,
    };
    if text.is_empty() && plan.is_none() {
        return Err("the model's reply had neither an answer nor a tool call".into());
    }
    Ok(Turn::Answer { text, plan })
}

/// Compact a tool result for the conversation.
pub fn tool_result_message(op: &str, result: &Value) -> String {
    let mut body = result.to_string();
    if body.len() > MAX_TOOL_RESULT_CHARS {
        let mut cut = MAX_TOOL_RESULT_CHARS;
        while !body.is_char_boundary(cut) {
            cut -= 1;
        }
        body.truncate(cut);
        body.push_str(" …[truncated — narrow the query with filters, fields or limit]");
    }
    format!("Result of {op}:\n{body}")
}

pub fn system_prompt(context: &Value) -> String {
    format!(
        r#"You are the drawing assistant inside Open CAD Studio, a DWG/DXF CAD program. Answer the user's questions about the OPEN drawing by querying it. Never guess counts, sizes or names: query first.

Reply with ONE JSON object per turn and nothing else, either a query:
  {{"tool": "<name>", "args": {{...}}}}
or the final answer:
  {{"answer": "<plain-text answer, with numbers and units>"}}
If the user asks you to change the drawing, you may add a plan they can review and apply:
  {{"answer": "...", "plan": {{"summary": "...", "steps": [ ... ]}}}}
Plan steps use the AICMD operations: entities_create, entities_transform (move/copy/rotate/scale/mirror/array), entities_delete (handles list or "$selection"), run ({{"cmd":"<command line>"}}), select.

Read-only tools:
- entities — counts by object type. No args.
- layers — layer table (name, on/off, frozen, locked, color) and the current layer. Args: offset, limit.
- header — units, current layer/text style, ltscale.
- query — find objects. Args (all optional, combined with AND): type (e.g. "Line", "Circle", "LwPolyline", "Insert", "Text"), layer, handles [..], detail ("summary"|"geometry"|"full"), limit (≤ 1000, default 200), offset, fields [property paths], bounds [x0,y0,x1,y1], near [x,y], contains_point [x,y], where [{{"path":"/radius","op":"gt","value":5}}] with ops eq ne lt lte gt gte contains starts_with ends_with in exists not_exists. Paths are JSON pointers into the serialized object, e.g. "/common/layer".
- measure — length, area and bounds. Args: handles [..] (default: the selection).
- audit — drawing integrity report.
- text_search — find text. Args: find, match_case, whole_word, layer.
- blocks — block definitions and how often each is inserted.
- get_selection — the objects the user has selected.

Use at most {max} queries per question. Prefer summary detail and counts over listing every object. Keep answers short and concrete.

Drawing context (JSON):
{context}"#,
        max = MAX_TOOL_CALLS,
        context = serde_json::to_string_pretty(context).unwrap_or_default()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tool_calls_and_strips_envelope_fields() {
        let t = parse_turn(r#"{"tool":"query","args":{"type":"Circle","document_id":4}}"#).unwrap();
        match t {
            Turn::Tool { op, request } => {
                assert_eq!(op, "query");
                assert_eq!(request["op"], "query");
                assert_eq!(request["limit"], 200);
                assert!(request.get("document_id").is_none());
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn refuses_mutating_tools() {
        assert!(parse_turn(r#"{"tool":"entities_delete","args":{"handles":["1"]}}"#).is_err());
        assert!(parse_turn(r#"{"tool":"save","args":{}}"#).is_err());
    }

    #[test]
    fn answers_with_and_without_plans() {
        match parse_turn(r#"{"answer":"There are 4 circles."}"#).unwrap() {
            Turn::Answer { text, plan } => {
                assert_eq!(text, "There are 4 circles.");
                assert!(plan.is_none());
            }
            other => panic!("{other:?}"),
        }
        let r = r#"{"answer":"I can purge them.","plan":{"summary":"erase","steps":[{"op":"entities_delete","handles":"$selection"}]}}"#;
        match parse_turn(r).unwrap() {
            Turn::Answer { plan: Some(p), .. } => assert_eq!(p.steps.len(), 1),
            other => panic!("{other:?}"),
        }
        match parse_turn("Just prose.").unwrap() {
            Turn::Answer { text, .. } => assert_eq!(text, "Just prose."),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn long_results_are_truncated_on_a_char_boundary() {
        let big = json!({"s": "é".repeat(MAX_TOOL_RESULT_CHARS)});
        let msg = tool_result_message("query", &big);
        assert!(msg.contains("truncated"));
        assert!(msg.len() < MAX_TOOL_RESULT_CHARS + 200);
    }
}
