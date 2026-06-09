use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Which API the user is talking to. OpenAI proper (api.openai.com, Bearer
/// auth, model in body) or Azure OpenAI (per-resource host, api-key auth,
/// deployment in the URL). The two differ in URL shape AND auth, so most of
/// the provider-specific logic lives in `provider.rs`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Openai,
    Azure,
}

impl Default for Provider {
    fn default() -> Self {
        Provider::Openai
    }
}

impl Provider {
    pub fn as_str(&self) -> &'static str {
        match self {
            Provider::Openai => "openai",
            Provider::Azure => "azure",
        }
    }
    pub fn parse(s: &str) -> Provider {
        match s.trim().to_lowercase().as_str() {
            "azure" => Provider::Azure,
            _ => Provider::Openai,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AppConfig {
    pub provider: Provider,

    // Shared secret. For OpenAI this is the sk-... key; for Azure the resource key.
    pub api_key: String,

    // OpenAI only: base URL up to and including `/v1`. Defaults to the public
    // API; override to point at a proxy (LiteLLM, corporate gateway) or any
    // OpenAI-compatible endpoint. Realtime reuses this host with a ws(s) scheme.
    pub openai_base_url: String,

    // Azure only: full resource endpoint, e.g. https://xxx.openai.azure.com
    pub azure_endpoint: String,
    pub azure_api_version: String,
    pub image_api_version: String,

    // Model/deployment names. For OpenAI these are model ids (gpt-realtime,
    // gpt-4o, gpt-image-1); for Azure they are deployment names.
    pub realtime_model: String,
    pub slide_model: String,
    pub image_model: String,

    pub voice: String,
    // Realtime-2 only. One of minimal|low|medium|high, or empty to omit.
    pub reasoning_effort: String,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            provider: Provider::Openai,
            api_key: String::new(),
            openai_base_url: "https://api.openai.com/v1".into(),
            azure_endpoint: String::new(),
            azure_api_version: "2024-10-01-preview".into(),
            image_api_version: "2025-04-01-preview".into(),
            // OpenAI defaults; user-editable in Settings.
            realtime_model: "gpt-realtime-2".into(),
            slide_model: "gpt-5.5".into(),
            image_model: "gpt-image-2".into(),
            voice: "alloy".into(),
            reasoning_effort: String::new(),
        }
    }
}

impl AppConfig {
    /// Enough configured to open a session?
    pub fn is_complete(&self) -> bool {
        if self.api_key.is_empty() {
            return false;
        }
        match self.provider {
            Provider::Openai => true,
            Provider::Azure => !self.azure_endpoint.is_empty(),
        }
    }
}

pub fn config_dir() -> Result<PathBuf, String> {
    let base = dirs_like::config_dir().ok_or("could not resolve OS config dir")?;
    Ok(base.join("slide-creator"))
}

pub fn env_path() -> Result<PathBuf, String> {
    Ok(config_dir()?.join(".env"))
}

/// Where generated decks are written (and served from). Per-OS data dir so a
/// packaged app can persist projects without touching the bundle.
pub fn decks_dir() -> Result<PathBuf, String> {
    Ok(config_dir()?.join("decks"))
}

pub fn load() -> AppConfig {
    let path = match env_path() {
        Ok(p) => p,
        Err(_) => return AppConfig::default(),
    };
    let raw = match fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => return AppConfig::default(),
    };
    from_map(&parse_dotenv(&raw))
}

pub fn save(cfg: &AppConfig) -> Result<(), String> {
    let dir = config_dir()?;
    fs::create_dir_all(&dir).map_err(|e| format!("mkdir {dir:?}: {e}"))?;
    let path = dir.join(".env");
    let mut out = String::new();
    out.push_str(&kv("PROVIDER", cfg.provider.as_str()));
    out.push_str(&kv("API_KEY", &cfg.api_key));
    out.push_str(&kv("OPENAI_BASE_URL", &cfg.openai_base_url));
    out.push_str(&kv("AZURE_OPENAI_ENDPOINT", &cfg.azure_endpoint));
    out.push_str(&kv("AZURE_OPENAI_API_VERSION", &cfg.azure_api_version));
    out.push_str(&kv("IMAGE_API_VERSION", &cfg.image_api_version));
    out.push_str(&kv("REALTIME_MODEL", &cfg.realtime_model));
    out.push_str(&kv("SLIDE_MODEL", &cfg.slide_model));
    out.push_str(&kv("IMAGE_MODEL", &cfg.image_model));
    out.push_str(&kv("VOICE", &cfg.voice));
    out.push_str(&kv("REASONING_EFFORT", &cfg.reasoning_effort));
    fs::write(&path, out).map_err(|e| format!("write {path:?}: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

fn kv(k: &str, v: &str) -> String {
    format!("{k}={}\n", v.replace('\n', " "))
}

fn parse_dotenv(raw: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            let v = v.trim();
            let v = v
                .strip_prefix('"')
                .and_then(|s| s.strip_suffix('"'))
                .or_else(|| v.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
                .unwrap_or(v);
            out.insert(k.trim().to_string(), v.to_string());
        }
    }
    out
}

fn from_map(m: &HashMap<String, String>) -> AppConfig {
    let mut c = AppConfig::default();
    if let Some(v) = m.get("PROVIDER") {
        c.provider = Provider::parse(v);
    }
    if let Some(v) = m.get("API_KEY") {
        c.api_key = v.clone();
    }
    if let Some(v) = m.get("OPENAI_BASE_URL") {
        let v = v.trim().trim_end_matches('/');
        if !v.is_empty() {
            c.openai_base_url = v.to_string();
        }
    }
    if let Some(v) = m.get("AZURE_OPENAI_ENDPOINT") {
        c.azure_endpoint = v.trim_end_matches('/').to_string();
    }
    if let Some(v) = m.get("AZURE_OPENAI_API_VERSION") {
        c.azure_api_version = v.clone();
    }
    if let Some(v) = m.get("IMAGE_API_VERSION") {
        c.image_api_version = v.clone();
    }
    if let Some(v) = m.get("REALTIME_MODEL") {
        c.realtime_model = v.clone();
    }
    if let Some(v) = m.get("SLIDE_MODEL") {
        c.slide_model = v.clone();
    }
    if let Some(v) = m.get("IMAGE_MODEL") {
        c.image_model = v.clone();
    }
    if let Some(v) = m.get("VOICE") {
        c.voice = v.clone();
    }
    if let Some(v) = m.get("REASONING_EFFORT") {
        c.reasoning_effort = v.clone();
    }
    c
}

mod dirs_like {
    use std::path::PathBuf;

    pub fn config_dir() -> Option<PathBuf> {
        #[cfg(target_os = "macos")]
        {
            let home = std::env::var_os("HOME")?;
            return Some(PathBuf::from(home).join("Library").join("Application Support"));
        }
        #[cfg(target_os = "windows")]
        {
            if let Some(v) = std::env::var_os("APPDATA") {
                return Some(PathBuf::from(v));
            }
            let home = std::env::var_os("USERPROFILE")?;
            return Some(PathBuf::from(home).join("AppData").join("Roaming"));
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            if let Some(v) = std::env::var_os("XDG_CONFIG_HOME") {
                return Some(PathBuf::from(v));
            }
            let home = std::env::var_os("HOME")?;
            return Some(PathBuf::from(home).join(".config"));
        }
    }
}
