//! AI provider settings: which providers are configured, which model each
//! feature uses, and the privacy / cost switches.
//!
//! Settings live in `<config>/ai_settings.json`. API keys are kept apart in
//! `<config>/ai_keys.json` (owner-only permissions on Unix) so the settings
//! file can be shared or attached to a bug report without leaking a key.
//! An environment variable (`OPENROUTER_API_KEY`, `OCS_OPENAI_API_KEY`)
//! overrides the stored key, which suits managed machines and CI.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};

pub const OPENROUTER_DEFAULT_URL: &str = "https://openrouter.ai/api/v1";
pub const OLLAMA_DEFAULT_URL: &str = "http://localhost:11434";
pub const CUSTOM_DEFAULT_URL: &str = "http://localhost:1234/v1";

/// A model provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ProviderKind {
    OpenRouter,
    Ollama,
    OpenAiCompatible,
}

impl ProviderKind {
    pub const ALL: [ProviderKind; 3] = [
        ProviderKind::OpenRouter,
        ProviderKind::Ollama,
        ProviderKind::OpenAiCompatible,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ProviderKind::OpenRouter => "OpenRouter",
            ProviderKind::Ollama => "Ollama (local)",
            ProviderKind::OpenAiCompatible => "OpenAI-compatible",
        }
    }

    /// Environment variable that overrides the stored key, if any.
    pub fn key_env_var(self) -> Option<&'static str> {
        match self {
            ProviderKind::OpenRouter => Some("OPENROUTER_API_KEY"),
            ProviderKind::OpenAiCompatible => Some("OCS_OPENAI_API_KEY"),
            ProviderKind::Ollama => None,
        }
    }
}

impl std::fmt::Display for ProviderKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// The AI-assisted features. Each can route to its own provider and model
/// (a cheap text model for chat, a vision model for image work, …).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum AiFeature {
    CommandBar,
    DrawingChat,
    ImageToModel,
    SketchToVector,
    DrawingChecker,
    ConstraintInference,
    TextToPart,
    FloorPlan,
    AutoDimension,
}

impl AiFeature {
    pub const ALL: [AiFeature; 9] = [
        AiFeature::CommandBar,
        AiFeature::DrawingChat,
        AiFeature::ImageToModel,
        AiFeature::SketchToVector,
        AiFeature::DrawingChecker,
        AiFeature::ConstraintInference,
        AiFeature::TextToPart,
        AiFeature::FloorPlan,
        AiFeature::AutoDimension,
    ];

    pub fn label(self) -> &'static str {
        match self {
            AiFeature::CommandBar => "Command bar (AICMD)",
            AiFeature::DrawingChat => "Drawing chat (AICHAT)",
            AiFeature::ImageToModel => "Image to model",
            AiFeature::SketchToVector => "Sketch to vector",
            AiFeature::DrawingChecker => "Drawing checker",
            AiFeature::ConstraintInference => "Constraint inference",
            AiFeature::TextToPart => "Text to part",
            AiFeature::FloorPlan => "Floor plan assistant",
            AiFeature::AutoDimension => "Auto dimensioning",
        }
    }

    /// Whether the feature sends images and so needs a vision model.
    pub fn needs_vision(self) -> bool {
        matches!(
            self,
            AiFeature::ImageToModel | AiFeature::SketchToVector | AiFeature::FloorPlan
        )
    }
}

/// Connection details of one provider (the key is stored separately).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProviderSettings {
    pub base_url: String,
    pub default_model: String,
}

/// A per-feature override; `None` / empty fields fall back to the defaults.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FeatureRoute {
    pub provider: Option<ProviderKind>,
    pub model: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AiSettings {
    pub default_provider: ProviderKind,
    pub openrouter: ProviderSettings,
    pub ollama: ProviderSettings,
    pub custom: ProviderSettings,
    pub features: BTreeMap<AiFeature, FeatureRoute>,
    /// Refuse any provider that is not on this machine.
    pub local_only: bool,
    /// Show a token / cost estimate and ask before each hosted request.
    pub confirm_before_send: bool,
    pub timeout_secs: u64,
    /// Cap on the reply length requested from the model.
    pub max_output_tokens: u32,
}

impl Default for AiSettings {
    fn default() -> Self {
        Self {
            default_provider: ProviderKind::OpenRouter,
            openrouter: ProviderSettings {
                base_url: OPENROUTER_DEFAULT_URL.into(),
                default_model: "anthropic/claude-sonnet-4.5".into(),
            },
            ollama: ProviderSettings {
                base_url: OLLAMA_DEFAULT_URL.into(),
                default_model: "qwen2.5vl:7b".into(),
            },
            custom: ProviderSettings {
                base_url: CUSTOM_DEFAULT_URL.into(),
                default_model: String::new(),
            },
            features: BTreeMap::new(),
            local_only: false,
            confirm_before_send: true,
            timeout_secs: 120,
            max_output_tokens: 4096,
        }
    }
}

impl Default for ProviderSettings {
    fn default() -> Self {
        Self {
            base_url: String::new(),
            default_model: String::new(),
        }
    }
}

/// Everything a request needs: where to send it, which model, which key.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedRoute {
    pub provider: ProviderKind,
    pub base_url: String,
    pub model: String,
    pub api_key: Option<String>,
    /// The endpoint is on this machine (no drawing data leaves it).
    pub is_local: bool,
    pub timeout_secs: u64,
    pub max_output_tokens: u32,
}

/// Whether a URL points at this machine.
pub fn is_local_url(url: &str) -> bool {
    let rest = url
        .trim()
        .strip_prefix("http://")
        .or_else(|| url.trim().strip_prefix("https://"))
        .unwrap_or(url.trim());
    let authority = rest.split('/').next().unwrap_or("");
    let host = if let Some(v6) = authority.strip_prefix('[') {
        v6.split(']').next().unwrap_or("")
    } else {
        authority.rsplit_once(':').map_or(authority, |(h, _)| h)
    };
    let host = host.to_ascii_lowercase();
    host == "localhost"
        || host.ends_with(".localhost")
        || host == "::1"
        || host.starts_with("127.")
        || host == "0.0.0.0"
}

impl AiSettings {
    pub fn provider(&self, kind: ProviderKind) -> &ProviderSettings {
        match kind {
            ProviderKind::OpenRouter => &self.openrouter,
            ProviderKind::Ollama => &self.ollama,
            ProviderKind::OpenAiCompatible => &self.custom,
        }
    }

    pub fn provider_mut(&mut self, kind: ProviderKind) -> &mut ProviderSettings {
        match kind {
            ProviderKind::OpenRouter => &mut self.openrouter,
            ProviderKind::Ollama => &mut self.ollama,
            ProviderKind::OpenAiCompatible => &mut self.custom,
        }
    }

    /// Whether requests to `kind` stay on this machine.
    pub fn provider_is_local(&self, kind: ProviderKind) -> bool {
        match kind {
            ProviderKind::OpenRouter => false,
            ProviderKind::Ollama | ProviderKind::OpenAiCompatible => {
                is_local_url(&self.provider(kind).base_url)
            }
        }
    }

    /// The provider and model `feature` uses, with its key, or why it can't
    /// be used (no model chosen, missing key, blocked by local-only).
    pub fn route(&self, feature: AiFeature, keys: &ApiKeys) -> Result<ResolvedRoute, String> {
        let over = self.features.get(&feature).cloned().unwrap_or_default();
        let provider = over.provider.unwrap_or(self.default_provider);
        let settings = self.provider(provider);
        let model = if over.model.trim().is_empty() {
            settings.default_model.trim().to_string()
        } else {
            over.model.trim().to_string()
        };
        self.resolve(provider, model, keys)
            .map_err(|e| format!("{}: {e}", feature.label()))
    }

    /// Resolve an explicit provider/model pair (used by "Test connection").
    pub fn resolve(
        &self,
        provider: ProviderKind,
        model: String,
        keys: &ApiKeys,
    ) -> Result<ResolvedRoute, String> {
        let settings = self.provider(provider);
        let base_url = settings.base_url.trim().trim_end_matches('/').to_string();
        if base_url.is_empty() {
            return Err(format!("no URL is set for {provider}"));
        }
        if model.is_empty() {
            return Err(format!(
                "no model is chosen for {provider}; pick one in AISETTINGS"
            ));
        }
        let is_local = self.provider_is_local(provider);
        if self.local_only && !is_local {
            return Err(format!(
                "{provider} is not on this machine and \"Only use local models\" is on"
            ));
        }
        let api_key = keys.get(provider);
        if provider == ProviderKind::OpenRouter && api_key.is_none() {
            return Err("OpenRouter needs an API key (AISETTINGS or OPENROUTER_API_KEY)".into());
        }
        Ok(ResolvedRoute {
            provider,
            base_url,
            model,
            api_key,
            is_local,
            timeout_secs: self.timeout_secs.clamp(5, 3600),
            max_output_tokens: self.max_output_tokens.clamp(64, 200_000),
        })
    }
}

/// Stored API keys. Never serialized into `ai_settings.json`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ApiKeys {
    pub openrouter: String,
    pub custom: String,
}

impl ApiKeys {
    /// The stored key for `kind`, or the env override when one is set.
    pub fn stored(&self, kind: ProviderKind) -> &str {
        match kind {
            ProviderKind::OpenRouter => &self.openrouter,
            ProviderKind::OpenAiCompatible => &self.custom,
            ProviderKind::Ollama => "",
        }
    }

    pub fn stored_mut(&mut self, kind: ProviderKind) -> Option<&mut String> {
        match kind {
            ProviderKind::OpenRouter => Some(&mut self.openrouter),
            ProviderKind::OpenAiCompatible => Some(&mut self.custom),
            ProviderKind::Ollama => None,
        }
    }

    /// Whether an environment variable supplies the key for `kind`.
    pub fn env_override(kind: ProviderKind) -> Option<String> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            kind.key_env_var()
                .and_then(|var| std::env::var(var).ok())
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = kind;
            None
        }
    }

    pub fn get(&self, kind: ProviderKind) -> Option<String> {
        Self::env_override(kind).or_else(|| {
            let v = self.stored(kind).trim();
            (!v.is_empty()).then(|| v.to_string())
        })
    }
}

// ── Persistence ─────────────────────────────────────────────────────────────

fn store() -> &'static Mutex<(AiSettings, ApiKeys)> {
    static STORE: OnceLock<Mutex<(AiSettings, ApiKeys)>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(load_from_disk()))
}

/// The current settings and keys (loaded from disk on first use).
pub fn current() -> (AiSettings, ApiKeys) {
    store()
        .lock()
        .map(|g| g.clone())
        .unwrap_or_else(|p| p.into_inner().clone())
}

/// Replace the settings and keys and write them to disk.
pub fn save(settings: AiSettings, keys: ApiKeys) -> Result<(), String> {
    let result = save_to_disk(&settings, &keys);
    match store().lock() {
        Ok(mut g) => *g = (settings, keys),
        Err(p) => *p.into_inner() = (settings, keys),
    }
    result
}

#[cfg(not(target_arch = "wasm32"))]
fn paths() -> Option<(std::path::PathBuf, std::path::PathBuf)> {
    let dir = crate::config::config_dir()?;
    Some((dir.join("ai_settings.json"), dir.join("ai_keys.json")))
}

#[cfg(not(target_arch = "wasm32"))]
fn load_from_disk() -> (AiSettings, ApiKeys) {
    let Some((settings_path, keys_path)) = paths() else {
        return Default::default();
    };
    let settings = std::fs::read_to_string(settings_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let keys = std::fs::read_to_string(keys_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    (settings, keys)
}

#[cfg(target_arch = "wasm32")]
fn load_from_disk() -> (AiSettings, ApiKeys) {
    Default::default()
}

#[cfg(not(target_arch = "wasm32"))]
fn save_to_disk(settings: &AiSettings, keys: &ApiKeys) -> Result<(), String> {
    let (settings_path, keys_path) =
        paths().ok_or_else(|| "the settings folder could not be found".to_string())?;
    if let Some(parent) = settings_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let body = serde_json::to_string_pretty(settings).map_err(|e| e.to_string())?;
    std::fs::write(&settings_path, body).map_err(|e| e.to_string())?;
    let body = serde_json::to_string_pretty(keys).map_err(|e| e.to_string())?;
    write_private(&keys_path, body.as_bytes()).map_err(|e| e.to_string())
}

#[cfg(target_arch = "wasm32")]
fn save_to_disk(_settings: &AiSettings, _keys: &ApiKeys) -> Result<(), String> {
    Ok(())
}

/// Write a file readable only by its owner where the platform supports it.
/// (On Windows the per-user `%APPDATA%` folder already restricts access.)
#[cfg(not(target_arch = "wasm32"))]
fn write_private(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        // An older file keeps its mode on open; tighten it explicitly.
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        file.write_all(bytes)
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_url_detection() {
        assert!(is_local_url("http://localhost:11434"));
        assert!(is_local_url("http://127.0.0.1:1234/v1"));
        assert!(is_local_url("http://[::1]:8080/v1"));
        assert!(is_local_url("https://LOCALHOST/"));
        assert!(!is_local_url("https://openrouter.ai/api/v1"));
        assert!(!is_local_url("http://192.168.1.20:11434"));
        assert!(!is_local_url("http://localhost.evil.com"));
    }

    #[test]
    fn feature_override_wins_over_default() {
        let mut s = AiSettings::default();
        s.default_provider = ProviderKind::Ollama;
        s.ollama.default_model = "llama3.2".into();
        s.features.insert(
            AiFeature::DrawingChat,
            FeatureRoute {
                provider: None,
                model: "qwen3:8b".into(),
            },
        );
        let keys = ApiKeys::default();
        assert_eq!(
            s.route(AiFeature::CommandBar, &keys).unwrap().model,
            "llama3.2"
        );
        assert_eq!(
            s.route(AiFeature::DrawingChat, &keys).unwrap().model,
            "qwen3:8b"
        );
    }

    #[test]
    fn local_only_blocks_hosted_providers() {
        let mut s = AiSettings::default();
        s.local_only = true;
        let keys = ApiKeys {
            openrouter: "sk-test".into(),
            custom: String::new(),
        };
        let err = s.route(AiFeature::CommandBar, &keys).unwrap_err();
        assert!(err.contains("local"), "{err}");
        s.default_provider = ProviderKind::Ollama;
        assert!(s.route(AiFeature::CommandBar, &keys).unwrap().is_local);
    }

    #[test]
    fn openrouter_requires_a_key() {
        let s = AiSettings::default();
        if ApiKeys::env_override(ProviderKind::OpenRouter).is_some() {
            return; // a developer machine with the env var set
        }
        assert!(s.route(AiFeature::CommandBar, &ApiKeys::default()).is_err());
    }

    #[test]
    fn keys_are_not_part_of_settings_json() {
        let json = serde_json::to_string(&AiSettings::default()).unwrap();
        assert!(!json.to_lowercase().contains("key"));
    }

    #[test]
    fn settings_roundtrip_through_json_with_defaults_for_missing_fields() {
        let s: AiSettings = serde_json::from_str(r#"{"local_only":true}"#).unwrap();
        assert!(s.local_only);
        assert_eq!(s.ollama.base_url, OLLAMA_DEFAULT_URL);
    }
}
