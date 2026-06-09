//! Provider-specific endpoint + auth construction.
//!
//! OpenAI proper and Azure OpenAI expose the same logical APIs (realtime voice,
//! chat-completions, image generation) but differ in two ways this module
//! centralizes:
//!   * URL shape — OpenAI: `https://api.openai.com/v1/<api>?model=...`;
//!     Azure: `https://<resource>/openai/deployments/<dep>/<api>?api-version=...`.
//!   * Auth — OpenAI: `Authorization: Bearer <key>` (+ a beta header for
//!     realtime); Azure: `api-key: <key>`.

use crate::config::{AppConfig, Provider};

/// OpenAI base URL with any trailing slash trimmed; falls back to the public
/// API if unset. This is the `/v1` root that all OpenAI endpoints hang off, so
/// pointing it at a proxy (LiteLLM, gateway, OpenAI-compatible server) reroutes
/// chat, images, and realtime together.
fn openai_base(cfg: &AppConfig) -> &str {
    let b = cfg.openai_base_url.trim().trim_end_matches('/');
    if b.is_empty() {
        "https://api.openai.com/v1"
    } else {
        b
    }
}

/// Headers (as (name, value) pairs) for an HTTPS request to this provider.
pub fn auth_headers(cfg: &AppConfig) -> Vec<(&'static str, String)> {
    match cfg.provider {
        Provider::Openai => vec![("Authorization", format!("Bearer {}", cfg.api_key))],
        Provider::Azure => vec![("api-key", cfg.api_key.clone())],
    }
}

/// Realtime WebSocket URL + the headers to open it with.
pub fn realtime_ws(cfg: &AppConfig) -> Result<(String, Vec<(&'static str, String)>), String> {
    match cfg.provider {
        Provider::Openai => {
            // Realtime shares the OpenAI base host/path but over WebSocket, so
            // swap the scheme: https->wss, http->ws (a plain-http proxy is
            // valid for local dev).
            let ws_base = ws_scheme(openai_base(cfg));
            let url = format!("{ws_base}/realtime?model={model}", model = cfg.realtime_model);
            let headers = vec![
                ("Authorization", format!("Bearer {}", cfg.api_key)),
                ("OpenAI-Beta", "realtime=v1".to_string()),
            ];
            Ok((url, headers))
        }
        Provider::Azure => {
            let host = azure_host(cfg)?;
            let url = format!(
                "wss://{host}/openai/realtime?api-version={api}&deployment={dep}",
                api = cfg.azure_api_version,
                dep = cfg.realtime_model,
            );
            Ok((url, vec![("api-key", cfg.api_key.clone())]))
        }
    }
}

/// chat/completions URL for the slide-building model.
pub fn chat_url(cfg: &AppConfig) -> Result<String, String> {
    match cfg.provider {
        Provider::Openai => Ok(format!("{}/chat/completions", openai_base(cfg))),
        Provider::Azure => {
            let host = azure_host(cfg)?;
            Ok(format!(
                "https://{host}/openai/deployments/{dep}/chat/completions?api-version={api}",
                dep = cfg.slide_model,
                api = cfg.azure_api_version,
            ))
        }
    }
}

/// images/generations URL for the image model.
pub fn image_url(cfg: &AppConfig) -> Result<String, String> {
    match cfg.provider {
        Provider::Openai => Ok(format!("{}/images/generations", openai_base(cfg))),
        Provider::Azure => {
            let host = azure_host(cfg)?;
            Ok(format!(
                "https://{host}/openai/deployments/{dep}/images/generations?api-version={api}",
                dep = cfg.image_model,
                api = cfg.image_api_version,
            ))
        }
    }
}

/// OpenAI puts the model in the request body; Azure puts it in the URL, so it
/// must be omitted from the body. Returns Some(model) only for OpenAI.
pub fn body_model<'a>(cfg: &'a AppConfig, model: &'a str) -> Option<&'a str> {
    match cfg.provider {
        Provider::Openai => Some(model),
        Provider::Azure => None,
    }
}

/// Convert an http(s) base URL to its ws(s) equivalent for the realtime socket.
fn ws_scheme(base: &str) -> String {
    if let Some(rest) = base.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = base.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        // No recognized scheme; assume secure.
        format!("wss://{base}")
    }
}

fn azure_host(cfg: &AppConfig) -> Result<String, String> {
    let parsed = url::Url::parse(&cfg.azure_endpoint)
        .map_err(|e| format!("Azure endpoint parse: {e}"))?;
    parsed
        .host_str()
        .map(|s| s.to_string())
        .ok_or_else(|| "Azure endpoint missing host".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AppConfig, Provider};

    fn openai_cfg(base: &str) -> AppConfig {
        let mut c = AppConfig::default();
        c.provider = Provider::Openai;
        c.openai_base_url = base.to_string();
        c.realtime_model = "gpt-realtime-2".into();
        c.slide_model = "gpt-5.5".into();
        c.image_model = "gpt-image-2".into();
        c
    }

    #[test]
    fn proxy_base_routes_all_endpoints() {
        let c = openai_cfg("https://litellm.stanford.edu/v1");
        assert_eq!(chat_url(&c).unwrap(), "https://litellm.stanford.edu/v1/chat/completions");
        assert_eq!(image_url(&c).unwrap(), "https://litellm.stanford.edu/v1/images/generations");
        let (ws, _) = realtime_ws(&c).unwrap();
        assert_eq!(ws, "wss://litellm.stanford.edu/v1/realtime?model=gpt-realtime-2");
    }

    #[test]
    fn default_base_when_empty() {
        let c = openai_cfg("");
        assert_eq!(chat_url(&c).unwrap(), "https://api.openai.com/v1/chat/completions");
        let (ws, _) = realtime_ws(&c).unwrap();
        assert!(ws.starts_with("wss://api.openai.com/v1/realtime"));
    }

    #[test]
    fn http_proxy_uses_ws() {
        let c = openai_cfg("http://localhost:4000/v1");
        let (ws, _) = realtime_ws(&c).unwrap();
        assert_eq!(ws, "ws://localhost:4000/v1/realtime?model=gpt-realtime-2");
    }
}
