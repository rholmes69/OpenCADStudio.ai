//! AI Assistant (AICMD / AICHAT): sends the conversation to the configured
//! model, runs AICHAT's read-only queries, and applies a reviewed AICMD plan
//! through the automation API as one undo step.

use super::{Message, ModalKind, OpenCADStudio};
use crate::ai::command_plan::{self, Plan};
use crate::ai::drawing_chat::{self, Turn};
use crate::ai::{ChatMessage, ChatRequest, ChatResponse};
use crate::ui::window::ai_assistant::{AiAssistMsg, EntryKind, Mode, State};
use iced::Task;
use serde_json::{json, Value};

/// Human name of a DWG `INSUNITS` code.
fn unit_name(code: i64) -> &'static str {
    match code {
        1 => "inches",
        2 => "feet",
        3 => "miles",
        4 => "millimeters",
        5 => "centimeters",
        6 => "meters",
        7 => "kilometers",
        8 => "microinches",
        9 => "mils",
        10 => "yards",
        14 => "decimeters",
        _ => "unitless",
    }
}

impl OpenCADStudio {
    pub(in crate::app) fn open_ai_assistant(&mut self, mode: Mode) {
        if self.tabs[self.active_tab].is_start {
            self.command_line
                .push_error("Open a drawing or start a new one before using the AI assistant.");
            return;
        }
        match self.ai_assistant.as_mut() {
            Some(state) if state.mode == mode => {}
            Some(state) if !state.busy => {
                *state = State::new(mode);
            }
            Some(_) => {}
            None => self.ai_assistant = Some(State::new(mode)),
        }
        self.active_modal = Some(ModalKind::AiAssistant);
        self.reset_modal_geometry();
        self.ai_refresh_estimate();
    }

    /// Rebuild the cached prompt and the footer estimate for the current input.
    fn ai_refresh_estimate(&mut self) {
        let Some((mode, input)) = self
            .ai_assistant
            .as_ref()
            .map(|s| (s.mode, s.input.clone()))
        else {
            return;
        };
        let _ = self.ai_system_prompt(mode);
        let estimate = self.ai_estimate(mode, &input);
        if let Some(state) = self.ai_assistant.as_mut() {
            state.estimate = estimate;
        }
    }

    /// What the model is told about the open drawing on every request.
    fn ai_drawing_context(&mut self) -> Value {
        let i = self.active_tab;
        let doc = &self.tabs[i].scene.document;
        let mut by_type: std::collections::BTreeMap<String, u64> = Default::default();
        let mut total = 0u64;
        for e in doc.entities() {
            *by_type
                .entry(crate::entities::names::ui_name(e).to_string())
                .or_default() += 1;
            total += 1;
        }
        let layers: Vec<Value> = doc
            .layers
            .iter()
            .take(200)
            .map(|l| {
                let mut flags = Vec::new();
                if l.is_off() {
                    flags.push("off");
                }
                if l.is_frozen() {
                    flags.push("frozen");
                }
                if l.is_locked() {
                    flags.push("locked");
                }
                if flags.is_empty() {
                    json!(l.name)
                } else {
                    json!(format!("{} ({})", l.name, flags.join(", ")))
                }
            })
            .collect();
        let units = doc.header.insertion_units as i64;
        let selected: Vec<String> = self.tabs[i]
            .scene
            .selected_handles_in_order()
            .iter()
            .map(|h| format!("{:X}", h.value()))
            .collect();
        let mut context = json!({
            "file": self.tabs[i].current_path.as_ref()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| self.tabs[i].tab_title.clone()),
            "units": unit_name(units),
            "current_layer": doc.header.current_layer_name,
            "layers": layers,
            "object_count": total,
            "objects_by_type": by_type,
            "current_space": self.tabs[i].scene.current_layout,
            "selection_count": selected.len(),
        });
        if !selected.is_empty() {
            context["selection_handles"] = json!(selected.iter().take(200).collect::<Vec<_>>());
        }
        // A few selected objects travel with their geometry so "array the
        // selected circle" needs no extra round trip.
        if (1..=20).contains(&selected.len()) {
            let answer = self.entity_query(
                i,
                &json!({"handles": selected, "detail": "geometry", "limit": 20}),
            );
            if answer["ok"] == true {
                context["selected_objects"] = answer["entities"].clone();
            }
        }
        context
    }

    fn ai_system_prompt(&mut self, mode: Mode) -> String {
        let context = self.ai_drawing_context();
        let system = match mode {
            Mode::Command => command_plan::system_prompt(&context),
            Mode::Chat => drawing_chat::system_prompt(&context),
        };
        if let Some(state) = self.ai_assistant.as_mut() {
            state.prompt_cache = system.clone();
        }
        system
    }

    fn ai_request(&mut self, state_mode: Mode, history: &[ChatMessage]) -> ChatRequest {
        let system = self.ai_system_prompt(state_mode);
        let mut messages = vec![ChatMessage::system(system)];
        messages.extend(history.iter().cloned());
        ChatRequest {
            messages,
            temperature: Some(0.1),
            json_mode: true,
        }
    }

    /// Send the conversation as it stands and route the reply back.
    fn ai_send_history(&mut self) -> Task<Message> {
        let Some(mode) = self.ai_assistant.as_ref().map(|s| s.mode) else {
            return Task::none();
        };
        let (settings, keys) = crate::ai::settings::current();
        let route = match settings.route(mode.feature(), &keys) {
            Ok(r) => r,
            Err(e) => {
                if let Some(state) = self.ai_assistant.as_mut() {
                    state.busy = false;
                    state.push(
                        EntryKind::Error,
                        format!("{e}. Open AISETTINGS to configure a provider."),
                    );
                }
                return Task::none();
            }
        };
        let history = self
            .ai_assistant
            .as_ref()
            .map(|s| s.history.clone())
            .unwrap_or_default();
        let request = self.ai_request(mode, &history);
        if let Some(state) = self.ai_assistant.as_mut() {
            state.busy = true;
        }
        Task::perform(
            crate::ai::run_blocking(move || crate::ai::client::chat(&route, &request)),
            |r| Message::AiAssist(AiAssistMsg::Reply(r)),
        )
    }

    pub(in crate::app) fn on_ai_assist(&mut self, msg: AiAssistMsg) -> Task<Message> {
        if self.ai_assistant.is_none() {
            return Task::none();
        }
        match msg {
            AiAssistMsg::Mode(mode) => {
                if let Some(state) = self.ai_assistant.as_mut() {
                    if !state.busy {
                        let input = std::mem::take(&mut state.input);
                        *state = State::new(mode);
                        state.input = input;
                    }
                }
                self.ai_refresh_estimate();
            }
            AiAssistMsg::Input(v) => {
                let mode = self
                    .ai_assistant
                    .as_ref()
                    .map(|s| s.mode)
                    .unwrap_or(Mode::Command);
                let estimate = self.ai_estimate(mode, &v);
                if let Some(state) = self.ai_assistant.as_mut() {
                    state.input = v;
                    state.confirm_armed = false;
                    state.estimate = estimate;
                }
            }
            AiAssistMsg::Send => return self.ai_send(),
            AiAssistMsg::Reply(result) => return self.ai_reply(result),
            AiAssistMsg::Apply => return self.ai_apply(),
            AiAssistMsg::Discard => {
                if let Some(state) = self.ai_assistant.as_mut() {
                    state.plan = None;
                    state.push(EntryKind::Info, "Plan discarded.");
                    state.history.push(ChatMessage::user(
                        "(I discarded that plan without applying it.)",
                    ));
                }
            }
            AiAssistMsg::Clear => {
                if let Some(state) = self.ai_assistant.as_mut() {
                    if !state.busy {
                        *state = State::new(state.mode);
                    }
                }
            }
            AiAssistMsg::OpenSettings => self.open_ai_settings(),
        }
        Task::none()
    }

    /// The footer estimate for sending `input` now.
    fn ai_estimate(&mut self, mode: Mode, input: &str) -> String {
        let (settings, keys) = crate::ai::settings::current();
        match settings.route(mode.feature(), &keys) {
            Ok(route) => {
                let mut history = self
                    .ai_assistant
                    .as_ref()
                    .map(|s| s.history.clone())
                    .unwrap_or_default();
                history.push(ChatMessage::user(input));
                let cached = self
                    .ai_assistant
                    .as_ref()
                    .map(|s| s.prompt_cache.clone())
                    .unwrap_or_default();
                let system = if cached.is_empty() {
                    self.ai_system_prompt(mode)
                } else {
                    cached
                };
                let mut messages = vec![ChatMessage::system(system)];
                messages.extend(history);
                let request = ChatRequest {
                    messages,
                    ..Default::default()
                };
                crate::ai::describe_estimate(&route, &request)
            }
            Err(e) => e,
        }
    }

    fn ai_send(&mut self) -> Task<Message> {
        let Some(state) = self.ai_assistant.as_ref() else {
            return Task::none();
        };
        let input = state.input.trim().to_string();
        if input.is_empty() || state.busy {
            return Task::none();
        }
        let mode = state.mode;
        // Hosted requests can cost money: the first Send shows the estimate
        // and arms; the second sends (AISETTINGS › Privacy & cost).
        let (settings, keys) = crate::ai::settings::current();
        let needs_confirm = settings.confirm_before_send
            && settings
                .route(mode.feature(), &keys)
                .is_ok_and(|route| !route.is_local);
        if needs_confirm && !state.confirm_armed {
            let estimate = self.ai_estimate(mode, &input);
            if let Some(state) = self.ai_assistant.as_mut() {
                state.estimate = estimate;
                state.confirm_armed = true;
            }
            return Task::none();
        }
        if let Some(state) = self.ai_assistant.as_mut() {
            state.confirm_armed = false;
            state.input.clear();
            state.tool_calls = 0;
            state.retries = 0;
            state.plan = None;
            state.push(EntryKind::User, input.clone());
            state.history.push(ChatMessage::user(input));
        }
        self.ai_send_history()
    }

    fn ai_reply(&mut self, result: Result<ChatResponse, String>) -> Task<Message> {
        let (settings, keys) = crate::ai::settings::current();
        let Some(state) = self.ai_assistant.as_mut() else {
            return Task::none();
        };
        state.busy = false;
        let response = match result {
            Ok(r) => r,
            Err(e) => {
                state.push(EntryKind::Error, e);
                return Task::none();
            }
        };
        if let Some(usage) = response.usage {
            state.tokens_in += usage.prompt_tokens;
            state.tokens_out += usage.completion_tokens;
            if let Ok(route) = settings.route(state.mode.feature(), &keys) {
                if let Some(cost) =
                    crate::ai::client::usage_cost_usd(&crate::ai::model_info(&route), usage)
                {
                    state.spent_usd += cost;
                }
            }
        }
        state
            .history
            .push(ChatMessage::assistant(response.content.clone()));
        let mode = state.mode;
        match mode {
            Mode::Command => match command_plan::parse_plan(&response.content) {
                Ok(plan) => {
                    self.ai_show_plan(plan);
                    Task::none()
                }
                Err(e) => self.ai_note_bad_reply(
                    format!("The model's plan could not be used: {e}"),
                    format!("Your plan was rejected: {e}. Answer again with a corrected JSON plan."),
                ),
            },
            Mode::Chat => match drawing_chat::parse_turn(&response.content) {
                Ok(Turn::Tool { op, request }) => self.ai_run_tool(op, request),
                Ok(Turn::Answer { text, plan }) => {
                    if let (false, Some(state)) = (text.is_empty(), self.ai_assistant.as_mut()) {
                        state.push(EntryKind::Assistant, text);
                    }
                    if let Some(plan) = plan {
                        self.ai_show_plan(plan);
                    }
                    Task::none()
                }
                Err(e) => self.ai_note_bad_reply(
                    e.clone(),
                    format!("That reply could not be used ({e}). Reply with a valid tool call or answer."),
                ),
            },
        }
    }

    /// Show why a reply was unusable and tell the model for its next try.
    /// Retries once automatically; after that the person decides.
    fn ai_note_bad_reply(&mut self, shown: String, to_model: String) -> Task<Message> {
        let Some(state) = self.ai_assistant.as_mut() else {
            return Task::none();
        };
        state.push(EntryKind::Error, shown);
        state.history.push(ChatMessage::user(to_model));
        if state.retries >= 1 {
            return Task::none();
        }
        state.retries += 1;
        state.push(EntryKind::Info, "Asking the model to correct its reply…");
        self.ai_send_history()
    }

    fn ai_show_plan(&mut self, plan: Plan) {
        let Some(state) = self.ai_assistant.as_mut() else {
            return;
        };
        if let Some(message) = &plan.message {
            state.push(EntryKind::Assistant, message.clone());
        }
        if plan.steps.is_empty() {
            return;
        }
        state.push(
            EntryKind::Assistant,
            if plan.summary.is_empty() {
                format!("Proposed a plan with {} step(s).", plan.steps.len())
            } else {
                format!(
                    "{} ({} step(s) — review below)",
                    plan.summary,
                    plan.steps.len()
                )
            },
        );
        state.plan = Some(plan);
    }

    /// Run one of AICHAT's read-only queries and hand the result back.
    fn ai_run_tool(&mut self, op: String, mut request: Value) -> Task<Message> {
        let calls = match self.ai_assistant.as_mut() {
            Some(state) => {
                state.tool_calls += 1;
                let args = {
                    let mut shown = request.clone();
                    if let Some(m) = shown.as_object_mut() {
                        m.remove("op");
                    }
                    shown.to_string()
                };
                state.push(
                    EntryKind::Tool,
                    if args == "{}" {
                        op.clone()
                    } else {
                        format!("{op} {args}")
                    },
                );
                state.tool_calls
            }
            None => return Task::none(),
        };
        request["protocol"] = json!(1);
        let result = self.ai_automation(&request);
        let mut message = drawing_chat::tool_result_message(&op, &result);
        if calls >= drawing_chat::MAX_TOOL_CALLS {
            message.push_str("\n\nThat was the last query allowed for this question. Answer now with what you have.");
        }
        if let Some(state) = self.ai_assistant.as_mut() {
            state.history.push(ChatMessage::user(message));
        }
        self.ai_send_history()
    }

    /// One automation-API request on the open drawing.
    fn ai_automation(&mut self, request: &Value) -> Value {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.automation_op(&request.to_string())
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = request;
            json!({"ok": false, "error": crate::ai::client::DESKTOP_ONLY})
        }
    }

    fn ai_apply(&mut self) -> Task<Message> {
        let Some(plan) = self.ai_assistant.as_mut().and_then(|s| s.plan.take()) else {
            return Task::none();
        };
        let outcome = self.apply_ai_plan(&plan);
        if let Some(state) = self.ai_assistant.as_mut() {
            match &outcome {
                Ok(report) => {
                    state.push(EntryKind::Info, report.clone());
                    state
                        .history
                        .push(ChatMessage::user(format!("(I applied the plan. {report})")));
                }
                Err(e) => {
                    state.push(EntryKind::Error, e.clone());
                    state.history.push(ChatMessage::user(format!(
                        "(Applying the plan failed and was rolled back: {e})"
                    )));
                }
            }
        }
        match outcome {
            Ok(report) => self.command_line.push_info(&format!("AICMD: {report}")),
            Err(e) => self.command_line.push_error(&format!("AICMD: {e}")),
        }
        self.ai_refresh_estimate();
        Task::none()
    }

    /// Apply `plan` step by step as ONE undo step. Each step records its
    /// own history as usual; the entries are then linked so a single Undo
    /// (or Redo) takes the whole plan. A failing step rolls everything back.
    pub(in crate::app) fn apply_ai_plan(&mut self, plan: &Plan) -> Result<String, String> {
        let i = self.active_tab;
        if self.tabs[i].is_start {
            return Err("no drawing is open".into());
        }
        if self.tabs[i].active_cmd.is_some() {
            return Err("finish or cancel the running command first".into());
        }
        let state = self.ai_automation(&json!({"protocol": 1, "op": "state"}));
        let document_id = state["document_id"].clone();
        let selection: Vec<String> = self.tabs[i]
            .scene
            .selected_handles_in_order()
            .iter()
            .map(|h| format!("{:X}", h.value()))
            .collect();
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();

        self.finish_pending_history(i);
        let revision_before = self.tabs[i].edit_revision;
        // Automation writes can be switched off for external clients; this
        // plan was reviewed and approved by the person at the screen.
        let control_was_enabled = self.control.enabled;
        self.control.enabled = true;

        let mut bindings = command_plan::Bindings {
            selection,
            ..Default::default()
        };
        let mut failure: Option<String> = None;
        let mut created = 0usize;
        for (n, step) in plan.steps.iter().enumerate() {
            let mut request = match command_plan::bind(step, &bindings) {
                Ok(r) => r,
                Err(e) => {
                    failure = Some(format!("step {}: {e}", n + 1));
                    break;
                }
            };
            if let Some(m) = request.as_object_mut() {
                m.remove("note");
            }
            request["protocol"] = json!(1);
            request["request_id"] = json!(format!("ai-{nonce}-{n}"));
            if !document_id.is_null() {
                request["document_id"] = document_id.clone();
            }
            let response = self.ai_automation(&request);
            self.finish_pending_history(i);
            let failed = response["ok"] == false || response["status"] == "failed";
            if failed {
                let reason = response["error"]
                    .as_str()
                    .or_else(|| response["message"].as_str())
                    .unwrap_or("the operation was refused");
                failure = Some(format!(
                    "step {} ({}): {reason}",
                    n + 1,
                    command_plan::describe_step(step)
                ));
                break;
            }
            // A command left waiting for input means the plan under-specified
            // it. (The response `status` also reads "waiting_input" while
            // this dialog is open, so only the command itself is checked.)
            if self.tabs[i].active_cmd.is_some() {
                let _ = self.ai_automation(&json!({
                    "protocol": 1,
                    "op": "cancel",
                    "request_id": format!("ai-{nonce}-{n}-cancel"),
                    "document_id": document_id,
                }));
                self.finish_pending_history(i);
                failure = Some(format!(
                    "step {} ({}): the command asked for more input than the plan gave it",
                    n + 1,
                    command_plan::describe_step(step)
                ));
                break;
            }
            let before = bindings.created.len();
            bindings.absorb(&response);
            created += bindings.created.len() - before;
        }
        self.control.enabled = control_was_enabled;

        // Every history entry pushed since the start belongs to the plan
        // (UNDO / REDO are refused in plans, so none were taken back).
        let pushed = (self.tabs[i].edit_revision.wrapping_sub(revision_before) as usize)
            .min(self.tabs[i].history.undo_stack.len());
        if let Some(e) = failure {
            if pushed > 0 {
                self.undo_steps_reported(pushed, false);
            }
            return Err(format!("{e}. Nothing was changed."));
        }
        self.link_last_undo_entries(i, "AICMD", pushed);
        Ok(format!(
            "applied {} step(s){}; one UNDO reverses it.",
            plan.steps.len(),
            if created > 0 {
                format!(", {created} new object(s)")
            } else {
                String::new()
            }
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::OpenCADStudio;
    use crate::ai::command_plan::parse_plan;

    fn count(app: &mut OpenCADStudio) -> u64 {
        app.automation_op(r#"{"op":"entities"}"#)["total"]
            .as_u64()
            .unwrap_or(0)
    }

    fn drawing() -> OpenCADStudio {
        let mut app = OpenCADStudio::new_for_test();
        app.automation_op(r#"{"op":"new"}"#);
        app
    }

    #[test]
    fn a_plan_applies_as_one_undo_step_while_the_dialog_is_open() {
        let mut app = drawing();
        app.active_modal = Some(super::ModalKind::AiAssistant);
        // External automation switched off must not block a reviewed plan.
        app.control.enabled = false;
        let plan = parse_plan(
            r#"{"summary":"circles and a line","steps":[
                {"op":"entities_create","entities":[{"type":"Circle","center":[0,0],"radius":5,"layer":"AI"}]},
                {"op":"entities_transform","handles":"$last","action":"copy","vector":[20,0]},
                {"op":"run","cmd":"LINE 0,0 10,0"},
                {"op":"entities_transform","handles":"$last","action":"move","vector":[0,30]}
            ]}"#,
        )
        .unwrap();
        let undo_before = app.tabs[app.active_tab].history.undo_stack.len();
        let report = app.apply_ai_plan(&plan).expect("plan applies");
        assert!(report.contains("4 step"), "{report}");
        assert_eq!(count(&mut app), 3);
        assert!(!app.control.enabled, "automation switch restored");
        let history = &app.tabs[app.active_tab].history;
        let labels: Vec<String> = history.undo_stack[undo_before..]
            .iter()
            .map(|h| h.label().to_string())
            .collect();
        assert!(!labels.is_empty());
        assert!(labels.iter().all(|l| l.starts_with("AICMD")), "{labels:?}");
        let lines = app.automation_op(r#"{"op":"query","type":"Line","detail":"geometry"}"#);
        assert_eq!(lines["count"], 1, "{lines}");
        // The person closes the dialog, then presses Undo.
        app.active_modal = None;
        app.automation_op(r#"{"op":"undo"}"#);
        assert_eq!(count(&mut app), 0, "one UNDO removes the whole plan");
        app.automation_op(r#"{"op":"redo"}"#);
        assert_eq!(count(&mut app), 3, "one REDO restores the whole plan");
        let moved = app.automation_op(r#"{"op":"query","type":"Line","detail":"geometry"}"#);
        assert_eq!(moved["entities"][0]["start"][1], 30.0, "{moved}");
        app.automation_op(r#"{"op":"undo"}"#);
        assert_eq!(count(&mut app), 0);
    }

    #[test]
    fn linked_labels_parse() {
        use crate::app::history::linked_step;
        assert_eq!(linked_step("AICMD (2/4)"), Some((2, 4)));
        assert_eq!(linked_step("AICMD (5/4)"), None);
        assert_eq!(linked_step("LINE"), None);
        assert_eq!(linked_step("Delete block X (1/2)"), None);
    }

    #[test]
    fn a_failing_step_rolls_the_plan_back() {
        let mut app = drawing();
        app.automation_op(r#"{"op":"run","cmd":"CIRCLE 50,50 2"}"#);
        let undo_before = app.tabs[app.active_tab].history.undo_stack.len();
        let plan = parse_plan(
            r#"{"steps":[
                {"op":"entities_create","entities":[{"type":"Line","start":[0,0],"end":[5,5]}]},
                {"op":"entities_delete","handles":["FFFFFF"]}
            ]}"#,
        )
        .unwrap();
        let err = app.apply_ai_plan(&plan).unwrap_err();
        assert!(err.contains("step 2"), "{err}");
        assert_eq!(count(&mut app), 1, "only the earlier circle remains");
        assert_eq!(
            app.tabs[app.active_tab].history.undo_stack.len(),
            undo_before,
            "the user's own history is untouched"
        );
    }

    #[test]
    fn a_plan_that_fails_first_does_not_undo_the_users_work() {
        let mut app = drawing();
        app.automation_op(r#"{"op":"run","cmd":"CIRCLE 0,0 2"}"#);
        let plan =
            parse_plan(r#"{"steps":[{"op":"entities_delete","handles":["FFFFFF"]}]}"#).unwrap();
        assert!(app.apply_ai_plan(&plan).is_err());
        assert_eq!(count(&mut app), 1);
    }

    #[test]
    fn context_describes_layers_counts_and_selection() {
        let mut app = drawing();
        app.automation_op(r#"{"op":"run","cmd":"CIRCLE 0,0 2"}"#);
        app.automation_op(r#"{"op":"select","type":"Circle"}"#);
        let context = app.ai_drawing_context();
        assert_eq!(context["object_count"], 1);
        assert!(context["layers"].as_array().is_some_and(|l| !l.is_empty()));
        assert_eq!(
            context["objects_by_type"].as_object().map(|m| m.len()),
            Some(1)
        );
        if context["selection_count"] == 1 {
            assert!(context["selected_objects"].is_array(), "{context}");
        }
    }
}
