//! Provider-neutral chat and model-list client.
//!
//! OpenRouter and generic OpenAI-compatible servers speak
//! `POST {base}/chat/completions`; Ollama is called through its native
//! `POST {base}/api/chat` (non-streaming), which takes images as plain base64
//! and works on every Ollama version, unlike its partial OpenAI shim.

use super::settings::{ProviderKind, ResolvedRoute};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const DESKTOP_ONLY: &str =
    "AI features need the desktop app: the web build cannot reach model providers directly";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
}

impl Role {
    fn as_str(self) -> &'static str {
        match self {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
        }
    }
}

/// An image attached to a message, already base64-encoded.
#[derive(Debug, Clone, PartialEq)]
pub struct ImageData {
    /// `image/png`, `image/jpeg`, …
    pub mime: String,
    pub base64: String,
}

impl ImageData {
    /// Encode raw file bytes, guessing the MIME type from the content.
    pub fn from_bytes(bytes: &[u8]) -> Self {
        use base64::Engine;
        let mime = match bytes {
            [0x89, b'P', b'N', b'G', ..] => "image/png",
            [0xFF, 0xD8, ..] => "image/jpeg",
            [b'G', b'I', b'F', ..] => "image/gif",
            [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => "image/webp",
            _ => "image/png",
        };
        Self {
            mime: mime.into(),
            base64: base64::engine::general_purpose::STANDARD.encode(bytes),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
    pub images: Vec<ImageData>,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: content.into(),
            images: Vec::new(),
        }
    }
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
            images: Vec::new(),
        }
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
            images: Vec::new(),
        }
    }
    pub fn with_image(mut self, image: ImageData) -> Self {
        self.images.push(image);
        self
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ChatRequest {
    pub messages: Vec<ChatMessage>,
    pub temperature: Option<f32>,
    /// Ask the provider for a JSON object reply where it supports that.
    pub json_mode: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Usage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChatResponse {
    pub content: String,
    pub model: String,
    pub usage: Option<Usage>,
}

/// A model a provider offers.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ModelInfo {
    pub id: String,
    pub name: String,
    pub context_length: Option<u64>,
    /// USD per prompt token (hosted providers only).
    pub prompt_price: Option<f64>,
    /// USD per completion token.
    pub completion_price: Option<f64>,
    /// Accepts image input (reported by OpenRouter; a name hint for Ollama).
    pub vision: bool,
}

/// Rough prompt size: ~4 characters per token plus a flat allowance per
/// image. Good enough for a pre-send estimate; the reply's `usage` is exact.
pub fn estimate_prompt_tokens(request: &ChatRequest) -> u64 {
    let chars: usize = request
        .messages
        .iter()
        .map(|m| m.content.chars().count() + 8)
        .sum();
    let images: usize = request.messages.iter().map(|m| m.images.len()).sum();
    (chars as u64).div_ceil(4) + images as u64 * 1100
}

/// Worst-case cost in USD of a request to `model` (prompt plus the full
/// output allowance), or `None` when the model has no published price.
pub fn estimate_cost_usd(model: &ModelInfo, prompt_tokens: u64, max_output: u64) -> Option<f64> {
    let p = model.prompt_price?;
    let c = model.completion_price.unwrap_or(p);
    Some(p * prompt_tokens as f64 + c * max_output as f64)
}

/// The exact cost of a finished request, when its model's price is known.
pub fn usage_cost_usd(model: &ModelInfo, usage: Usage) -> Option<f64> {
    let p = model.prompt_price?;
    let c = model.completion_price.unwrap_or(p);
    Some(p * usage.prompt_tokens as f64 + c * usage.completion_tokens as f64)
}

// ── Request bodies (pure, unit-tested) ──────────────────────────────────────

fn openai_body(route: &ResolvedRoute, request: &ChatRequest) -> Value {
    let messages: Vec<Value> = request
        .messages
        .iter()
        .map(|m| {
            if m.images.is_empty() {
                json!({ "role": m.role.as_str(), "content": m.content })
            } else {
                let mut parts = vec![json!({ "type": "text", "text": m.content })];
                parts.extend(m.images.iter().map(|img| {
                    json!({
                        "type": "image_url",
                        "image_url": { "url": format!("data:{};base64,{}", img.mime, img.base64) }
                    })
                }));
                json!({ "role": m.role.as_str(), "content": parts })
            }
        })
        .collect();
    let mut body = json!({
        "model": route.model,
        "messages": messages,
        "max_tokens": route.max_output_tokens,
        "stream": false,
    });
    if let Some(t) = request.temperature {
        body["temperature"] = json!(t);
    }
    if request.json_mode {
        body["response_format"] = json!({ "type": "json_object" });
    }
    body
}

fn ollama_body(route: &ResolvedRoute, request: &ChatRequest) -> Value {
    let messages: Vec<Value> = request
        .messages
        .iter()
        .map(|m| {
            let mut msg = json!({ "role": m.role.as_str(), "content": m.content });
            if !m.images.is_empty() {
                msg["images"] = json!(m
                    .images
                    .iter()
                    .map(|i| i.base64.clone())
                    .collect::<Vec<_>>());
            }
            msg
        })
        .collect();
    let mut options = json!({ "num_predict": route.max_output_tokens });
    if let Some(t) = request.temperature {
        options["temperature"] = json!(t);
    }
    let mut body = json!({
        "model": route.model,
        "messages": messages,
        "stream": false,
        "options": options,
    });
    if request.json_mode {
        body["format"] = json!("json");
    }
    body
}

fn parse_openai_reply(body: &Value) -> Result<ChatResponse, String> {
    if let Some(err) = body.get("error") {
        return Err(error_text(err));
    }
    let content = body
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .ok_or_else(|| "the provider's reply had no message content".to_string())?
        .to_string();
    let usage = body.get("usage").map(|u| Usage {
        prompt_tokens: u.get("prompt_tokens").and_then(Value::as_u64).unwrap_or(0),
        completion_tokens: u
            .get("completion_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
    });
    Ok(ChatResponse {
        content,
        model: body
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        usage,
    })
}

fn parse_ollama_reply(body: &Value) -> Result<ChatResponse, String> {
    if let Some(err) = body.get("error") {
        return Err(error_text(err));
    }
    let content = body
        .pointer("/message/content")
        .and_then(Value::as_str)
        .ok_or_else(|| "Ollama's reply had no message content".to_string())?
        .to_string();
    let usage = match (
        body.get("prompt_eval_count").and_then(Value::as_u64),
        body.get("eval_count").and_then(Value::as_u64),
    ) {
        (None, None) => None,
        (p, c) => Some(Usage {
            prompt_tokens: p.unwrap_or(0),
            completion_tokens: c.unwrap_or(0),
        }),
    };
    Ok(ChatResponse {
        content,
        model: body
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        usage,
    })
}

fn error_text(err: &Value) -> String {
    err.get("message")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| err.as_str().map(str::to_string))
        .unwrap_or_else(|| err.to_string())
}

fn looks_like_vision(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    [
        "vision",
        "-vl",
        "vl:",
        "llava",
        "minicpm-v",
        "gemma3",
        "moondream",
        "bakllava",
        "pixtral",
        "qwen2.5vl",
        "qwen3-vl",
        "granite3.2-vision",
        "llama4",
    ]
    .iter()
    .any(|hint| n.contains(hint))
}

fn parse_price(v: Option<&Value>) -> Option<f64> {
    let v = v?;
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        .filter(|p: &f64| p.is_finite() && *p >= 0.0)
}

fn parse_openai_models(body: &Value) -> Vec<ModelInfo> {
    let mut models: Vec<ModelInfo> = body
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|m| {
            let id = m.get("id")?.as_str()?.to_string();
            let vision = m
                .pointer("/architecture/input_modalities")
                .and_then(Value::as_array)
                .map(|mods| mods.iter().any(|x| x.as_str() == Some("image")))
                .unwrap_or_else(|| looks_like_vision(&id));
            Some(ModelInfo {
                name: m
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or(&id)
                    .to_string(),
                context_length: m.get("context_length").and_then(Value::as_u64),
                prompt_price: parse_price(m.pointer("/pricing/prompt")),
                completion_price: parse_price(m.pointer("/pricing/completion")),
                vision,
                id,
            })
        })
        .collect();
    models.sort_by(|a, b| a.id.cmp(&b.id));
    models
}

fn parse_ollama_models(body: &Value) -> Vec<ModelInfo> {
    let mut models: Vec<ModelInfo> = body
        .get("models")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|m| {
            let id = m
                .get("model")
                .or_else(|| m.get("name"))?
                .as_str()?
                .to_string();
            let family = m
                .pointer("/details/families")
                .and_then(Value::as_array)
                .map(|f| {
                    f.iter()
                        .any(|x| matches!(x.as_str(), Some("clip" | "mllama")))
                })
                .unwrap_or(false);
            Some(ModelInfo {
                name: id.clone(),
                vision: family || looks_like_vision(&id),
                prompt_price: Some(0.0),
                completion_price: Some(0.0),
                context_length: None,
                id,
            })
        })
        .collect();
    models.sort_by(|a, b| a.id.cmp(&b.id));
    models
}

// ── Network (native only) ───────────────────────────────────────────────────

#[cfg(not(target_arch = "wasm32"))]
mod net {
    use super::*;
    use std::time::Duration;

    const USER_AGENT: &str = concat!("OpenCADStudio/", env!("OCS_APP_VERSION"));

    fn add_headers<B>(
        mut req: ureq::RequestBuilder<B>,
        provider: ProviderKind,
        api_key: Option<&str>,
    ) -> ureq::RequestBuilder<B> {
        req = req.header("User-Agent", USER_AGENT);
        if let Some(key) = api_key.filter(|k| !k.is_empty()) {
            req = req.header("Authorization", &format!("Bearer {key}"));
        }
        if provider == ProviderKind::OpenRouter {
            // OpenRouter's optional app attribution headers.
            req = req
                .header("HTTP-Referer", "https://www.opencadstudio.com")
                .header("X-Title", "Open CAD Studio");
        }
        req
    }

    fn read_json(mut resp: ureq::http::Response<ureq::Body>) -> Result<Value, String> {
        let status = resp.status().as_u16();
        let text = resp
            .body_mut()
            .with_config()
            .limit(32 * 1024 * 1024)
            .read_to_string()
            .map_err(|e| e.to_string())?;
        let parsed: Result<Value, _> = serde_json::from_str(&text);
        if !(200..300).contains(&status) {
            let detail = parsed
                .ok()
                .and_then(|v| v.get("error").map(error_text))
                .unwrap_or_else(|| text.chars().take(300).collect());
            return Err(match status {
                401 | 403 => format!("the provider refused the API key (HTTP {status}): {detail}"),
                404 => format!("not found (HTTP 404) — check the URL and model name: {detail}"),
                429 => format!("rate limited (HTTP 429): {detail}"),
                _ => format!("HTTP {status}: {detail}"),
            });
        }
        parsed.map_err(|e| format!("the provider sent invalid JSON: {e}"))
    }

    fn friendly(provider: ProviderKind, base: &str, e: ureq::Error) -> String {
        let s = e.to_string();
        if provider == ProviderKind::Ollama
            && (s.contains("onnect") || s.contains("refused") || s.contains("resolve"))
        {
            format!("could not reach Ollama at {base} — is `ollama serve` running? ({s})")
        } else {
            s
        }
    }

    pub fn chat(route: &ResolvedRoute, request: &ChatRequest) -> Result<ChatResponse, String> {
        let agent = crate::network::agent(Duration::from_secs(route.timeout_secs));
        let (url, body) = match route.provider {
            ProviderKind::Ollama => (
                format!("{}/api/chat", route.base_url),
                ollama_body(route, request),
            ),
            _ => (
                format!("{}/chat/completions", route.base_url),
                openai_body(route, request),
            ),
        };
        let req = agent
            .post(&url)
            .config()
            .http_status_as_error(false)
            .build()
            .content_type("application/json");
        let req = add_headers(req, route.provider, route.api_key.as_deref());
        let resp = req
            .send(body.to_string())
            .map_err(|e| friendly(route.provider, &route.base_url, e))?;
        let json = read_json(resp)?;
        match route.provider {
            ProviderKind::Ollama => parse_ollama_reply(&json),
            _ => parse_openai_reply(&json),
        }
    }

    pub fn list_models(
        provider: ProviderKind,
        base_url: &str,
        api_key: Option<&str>,
    ) -> Result<Vec<ModelInfo>, String> {
        let base = base_url.trim().trim_end_matches('/');
        if base.is_empty() {
            return Err("no URL is set".into());
        }
        let agent = crate::network::agent(Duration::from_secs(20));
        let url = match provider {
            ProviderKind::Ollama => format!("{base}/api/tags"),
            _ => format!("{base}/models"),
        };
        let req = agent.get(&url).config().http_status_as_error(false).build();
        let req = add_headers(req, provider, api_key);
        let resp = req.call().map_err(|e| friendly(provider, base, e))?;
        let json = read_json(resp)?;
        Ok(match provider {
            ProviderKind::Ollama => parse_ollama_models(&json),
            _ => parse_openai_models(&json),
        })
    }
}

/// Send `request` along `route` and wait for the whole reply. Blocking:
/// call it inside [`super::run_blocking`].
pub fn chat(route: &ResolvedRoute, request: &ChatRequest) -> Result<ChatResponse, String> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        net::chat(route, request)
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (route, request);
        Err(DESKTOP_ONLY.into())
    }
}

/// The models `provider` offers. Blocking.
pub fn list_models(
    provider: ProviderKind,
    base_url: &str,
    api_key: Option<&str>,
) -> Result<Vec<ModelInfo>, String> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        net::list_models(provider, base_url, api_key)
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (provider, base_url, api_key);
        Err(DESKTOP_ONLY.into())
    }
}

/// Pull the first JSON object or array out of a model reply that may wrap it
/// in prose or a ```json fence.
pub fn extract_json(reply: &str) -> Option<Value> {
    let trimmed = reply.trim();
    if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
        return Some(v);
    }
    if let Some(fenced) = trimmed.split("```").nth(1) {
        let inner = fenced.strip_prefix("json").unwrap_or(fenced).trim();
        if let Ok(v) = serde_json::from_str::<Value>(inner) {
            return Some(v);
        }
    }
    let start = trimmed.find(['{', '['])?;
    let open = trimmed.as_bytes()[start];
    let close = if open == b'{' { '}' } else { ']' };
    let end = trimmed.rfind(close)?;
    serde_json::from_str(&trimmed[start..=end]).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(provider: ProviderKind) -> ResolvedRoute {
        ResolvedRoute {
            provider,
            base_url: "http://x".into(),
            model: "m".into(),
            api_key: None,
            is_local: true,
            timeout_secs: 30,
            max_output_tokens: 500,
        }
    }

    #[test]
    fn openai_body_uses_content_parts_for_images() {
        let req = ChatRequest {
            messages: vec![
                ChatMessage::system("s"),
                ChatMessage::user("look").with_image(ImageData {
                    mime: "image/png".into(),
                    base64: "AAA".into(),
                }),
            ],
            temperature: Some(0.2),
            json_mode: true,
        };
        let body = openai_body(&route(ProviderKind::OpenRouter), &req);
        assert_eq!(body["messages"][0]["content"], "s");
        assert_eq!(body["messages"][1]["content"][1]["type"], "image_url");
        assert_eq!(
            body["messages"][1]["content"][1]["image_url"]["url"],
            "data:image/png;base64,AAA"
        );
        assert_eq!(body["response_format"]["type"], "json_object");
        assert_eq!(body["max_tokens"], 500);
    }

    #[test]
    fn ollama_body_uses_bare_base64_images_and_format_json() {
        let req = ChatRequest {
            messages: vec![ChatMessage::user("look").with_image(ImageData {
                mime: "image/png".into(),
                base64: "AAA".into(),
            })],
            temperature: None,
            json_mode: true,
        };
        let body = ollama_body(&route(ProviderKind::Ollama), &req);
        assert_eq!(body["messages"][0]["images"][0], "AAA");
        assert_eq!(body["format"], "json");
        assert_eq!(body["stream"], false);
        assert_eq!(body["options"]["num_predict"], 500);
    }

    #[test]
    fn parses_replies_and_errors() {
        let ok = json!({"model":"a","choices":[{"message":{"content":"hi"}}],"usage":{"prompt_tokens":3,"completion_tokens":1}});
        let r = parse_openai_reply(&ok).unwrap();
        assert_eq!(r.content, "hi");
        assert_eq!(r.usage.unwrap().prompt_tokens, 3);
        let err = json!({"error":{"message":"bad key"}});
        assert_eq!(parse_openai_reply(&err).unwrap_err(), "bad key");
        let ol =
            json!({"model":"q","message":{"content":"yo"},"prompt_eval_count":5,"eval_count":2});
        assert_eq!(
            parse_ollama_reply(&ol)
                .unwrap()
                .usage
                .unwrap()
                .completion_tokens,
            2
        );
    }

    #[test]
    fn parses_model_lists() {
        let or = json!({"data":[
            {"id":"b/text","name":"Text","pricing":{"prompt":"0.000003","completion":"0.000015"},"architecture":{"input_modalities":["text"]}},
            {"id":"a/vis","pricing":{"prompt":"0","completion":"0"},"architecture":{"input_modalities":["text","image"]}}
        ]});
        let m = parse_openai_models(&or);
        assert_eq!(m[0].id, "a/vis");
        assert!(m[0].vision);
        assert!(!m[1].vision);
        assert!((m[1].prompt_price.unwrap() - 3e-6).abs() < 1e-12);
        let ol = json!({"models":[{"name":"llama3.2:latest","model":"llama3.2:latest"},{"model":"qwen2.5vl:7b"}]});
        let m = parse_ollama_models(&ol);
        assert_eq!(m.len(), 2);
        assert!(m.iter().any(|x| x.id == "qwen2.5vl:7b" && x.vision));
    }

    #[test]
    fn estimates() {
        let req = ChatRequest {
            messages: vec![ChatMessage::user("x".repeat(392))],
            ..Default::default()
        };
        assert_eq!(estimate_prompt_tokens(&req), 100);
        let model = ModelInfo {
            prompt_price: Some(1e-6),
            completion_price: Some(2e-6),
            ..Default::default()
        };
        assert!((estimate_cost_usd(&model, 1000, 500).unwrap() - 0.002).abs() < 1e-12);
    }

    #[test]
    fn extracts_json_from_wrapped_replies() {
        assert_eq!(extract_json("{\"a\":1}").unwrap()["a"], 1);
        assert_eq!(
            extract_json("Sure:\n```json\n{\"a\":2}\n```").unwrap()["a"],
            2
        );
        assert_eq!(
            extract_json("Here you go {\"a\":3} hope it helps").unwrap()["a"],
            3
        );
        assert!(extract_json("no json here").is_none());
    }

    /// A one-shot HTTP server on localhost answering `body` to one request;
    /// the thread returns the raw request it received.
    #[cfg(not(target_arch = "wasm32"))]
    fn one_shot_server(body: &'static str) -> (String, std::thread::JoinHandle<String>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let n = stream.read(&mut chunk).unwrap();
                buf.extend_from_slice(&chunk[..n]);
                let text = String::from_utf8_lossy(&buf).to_string();
                if let Some(end) = text.find("\r\n\r\n") {
                    let len = text
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                        })
                        .unwrap_or(0);
                    if buf.len() >= end + 4 + len || n == 0 {
                        break;
                    }
                }
                if n == 0 {
                    break;
                }
            }
            let reply = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(reply.as_bytes()).unwrap();
            String::from_utf8_lossy(&buf).to_string()
        });
        (base, handle)
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn chat_round_trip_against_a_local_openai_compatible_server() {
        let (base, server) = one_shot_server(
            r#"{"model":"m","choices":[{"message":{"content":"pong"}}],"usage":{"prompt_tokens":7,"completion_tokens":1}}"#,
        );
        let mut r = route(ProviderKind::OpenAiCompatible);
        r.base_url = format!("{base}/v1");
        r.api_key = Some("secret-key".into());
        let reply = chat(
            &r,
            &ChatRequest {
                messages: vec![ChatMessage::user("ping")],
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(reply.content, "pong");
        assert_eq!(reply.usage.unwrap().prompt_tokens, 7);
        let request = server.join().unwrap();
        assert!(
            request.starts_with("POST /v1/chat/completions"),
            "{request}"
        );
        assert!(request.contains("Bearer secret-key"));
        assert!(request.contains("\"ping\""));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn ollama_chat_and_model_list_use_the_native_api() {
        let (base, server) =
            one_shot_server(r#"{"model":"llama3.2","message":{"content":"hi"},"eval_count":1}"#);
        let mut r = route(ProviderKind::Ollama);
        r.base_url = base;
        let reply = chat(
            &r,
            &ChatRequest {
                messages: vec![ChatMessage::user("x")],
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(reply.content, "hi");
        assert!(server.join().unwrap().starts_with("POST /api/chat"));

        let (base, server) = one_shot_server(r#"{"models":[{"model":"llava:7b"}]}"#);
        let models = list_models(ProviderKind::Ollama, &base, None).unwrap();
        assert!(models[0].vision);
        assert!(server.join().unwrap().starts_with("GET /api/tags"));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn unreachable_ollama_gets_a_helpful_error() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        drop(listener);
        let err = list_models(ProviderKind::Ollama, &base, None).unwrap_err();
        assert!(err.contains("ollama serve"), "{err}");
    }

    #[test]
    fn image_mime_sniffing() {
        assert_eq!(
            ImageData::from_bytes(&[0xFF, 0xD8, 0xFF]).mime,
            "image/jpeg"
        );
        assert_eq!(ImageData::from_bytes(b"\x89PNG\r\n").mime, "image/png");
    }
}
