//! Shared AI provider layer.
//!
//! Every AI-assisted feature (the natural-language command bar, drawing chat,
//! image-to-model, …) reaches a language model through this module instead of
//! talking HTTP itself. That keeps provider choice, API keys, the local-only
//! privacy switch and cost estimates in one place, configured once from the
//! `AISETTINGS` dialog.
//!
//! Three provider kinds are supported:
//!
//! * **OpenRouter** — one key for hosted models (Claude, GPT, Gemini, Qwen, …).
//! * **Ollama** — a local server (`http://localhost:11434` by default) for
//!   offline or private work.
//! * **OpenAI-compatible** — any other `/v1/chat/completions` endpoint
//!   (LM Studio, vLLM, llama.cpp server, a company gateway).
//!
//! Requests are blocking and run on a worker thread through [`run_blocking`],
//! so the UI never waits on the network. The web build has no direct network
//! path to these providers (CORS, no localhost access), so the client returns
//! a "desktop only" error there and the dialogs say so.

pub mod client;
pub mod command_plan;
pub mod constraint_suggest;
pub mod drawing_chat;
pub mod drawing_check;
pub mod image_to_model;
pub mod settings;

pub use client::{
    estimate_cost_usd, estimate_prompt_tokens, ChatMessage, ChatRequest, ChatResponse, ImageData,
    ModelInfo, Role, Usage,
};
pub use settings::{AiFeature, AiSettings, ProviderKind, ResolvedRoute};

/// Model lists fetched in this session, per provider, so features can show
/// prices and vision support without asking the provider again.
fn model_cache(
) -> &'static std::sync::Mutex<std::collections::HashMap<ProviderKind, Vec<ModelInfo>>> {
    static CACHE: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<ProviderKind, Vec<ModelInfo>>>,
    > = std::sync::OnceLock::new();
    CACHE.get_or_init(Default::default)
}

pub fn cached_models(provider: ProviderKind) -> Vec<ModelInfo> {
    model_cache()
        .lock()
        .ok()
        .and_then(|c| c.get(&provider).cloned())
        .unwrap_or_default()
}

pub fn set_cached_models(provider: ProviderKind, models: Vec<ModelInfo>) {
    if let Ok(mut c) = model_cache().lock() {
        c.insert(provider, models);
    }
}

/// What is known about the model a route points at (price, vision). Ollama
/// and local servers are free; an unlisted hosted model has no price.
pub fn model_info(route: &ResolvedRoute) -> ModelInfo {
    cached_models(route.provider)
        .into_iter()
        .find(|m| m.id == route.model)
        .unwrap_or_else(|| ModelInfo {
            id: route.model.clone(),
            name: route.model.clone(),
            prompt_price: route.is_local.then_some(0.0),
            completion_price: route.is_local.then_some(0.0),
            ..Default::default()
        })
}

/// A one-line pre-send summary: model, estimated tokens and worst-case cost.
pub fn describe_estimate(route: &ResolvedRoute, request: &ChatRequest) -> String {
    let tokens = estimate_prompt_tokens(request);
    let info = model_info(route);
    let cost = if route.is_local {
        "free (local)".to_string()
    } else {
        match estimate_cost_usd(&info, tokens, route.max_output_tokens as u64) {
            Some(c) => format!("up to ${c:.4}"),
            None => "price unknown (refresh models in AISETTINGS)".to_string(),
        }
    };
    format!(
        "{} · {} · ~{tokens} prompt tokens, ≤{} reply tokens · {cost}",
        route.provider, route.model, route.max_output_tokens
    )
}

/// Run `work` off the UI thread and resolve with its result.
///
/// Native builds use a plain worker thread and a oneshot channel (the same
/// pattern the Start page feeds use); the web build can't spawn threads and
/// has no provider access, so it resolves immediately with an error.
pub fn run_blocking<T, F>(work: F) -> impl std::future::Future<Output = Result<T, String>>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, String> + Send + 'static,
{
    #[cfg(not(target_arch = "wasm32"))]
    {
        let (tx, rx) = iced::futures::channel::oneshot::channel();
        std::thread::spawn(move || {
            let _ = tx.send(work());
        });
        async move {
            rx.await
                .unwrap_or_else(|_| Err("AI worker thread stopped unexpectedly".to_string()))
        }
    }
    #[cfg(target_arch = "wasm32")]
    {
        drop(work);
        std::future::ready(Err(client::DESKTOP_ONLY.to_string()))
    }
}
