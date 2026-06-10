use std::path::PathBuf;
use std::sync::Arc;

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, State,
    },
    http::StatusCode,
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::{mpsc, Mutex};
use tower_http::services::ServeDir;

use crate::config::{AppConfig, Provider};
use crate::deck::{render_deck_editor_html, render_deck_html, render_deck_pdf_html, Deck};
use crate::images::{ImageStore, MAX_IMAGE_BYTES};
use crate::realtime::{self, SessionOptions};
use crate::refs::{RefStore, MAX_FILE_BYTES};
use crate::slide_agent::{Emit, SlideAgent};

#[derive(Clone)]
pub struct AppState {
    pub web_dir: PathBuf,
    /// Root of all project state (`<data>/decks`). Per-deck files live under
    /// `decks_dir/projects/<id>/`; resolve the active one via `project_dir()`.
    pub decks_dir: PathBuf,
    pub cfg: Arc<Mutex<Option<AppConfig>>>,
    /// Handle to the Tauri app, used to spawn secondary windows (editor, deck
    /// preview) from `/api/open-window`. `None` outside the desktop app (e.g.
    /// the Python-parity integration tests), where the route is unused.
    pub app: Option<tauri::AppHandle>,
}

impl AppState {
    fn projects(&self) -> crate::projects::ProjectStore {
        crate::projects::ProjectStore::new(&self.decks_dir)
    }
    /// Directory of the active project — where deck.json/deck.html/refs/images
    /// for the current deck live. Every per-deck handler routes through here.
    fn project_dir(&self) -> PathBuf {
        self.projects()
            .active_dir()
            .unwrap_or_else(|_| self.decks_dir.join("projects").join("default"))
    }
}

pub async fn build_router(state: AppState) -> Router {
    let web = ServeDir::new(state.web_dir.clone()).append_index_html_on_directories(true);

    Router::new()
        .route("/healthz", get(healthz))
        .route("/ws", get(ws_handler))
        .route("/api/config", get(get_config).post(post_config))
        .route("/api/projects", get(get_projects).post(post_project))
        .route(
            "/api/projects/:id",
            axum::routing::patch(patch_project).delete(delete_project),
        )
        .route("/api/projects/:id/activate", axum::routing::post(activate_project))
        .route("/editor", get(get_editor))
        .route("/api/deck", get(get_deck).post(post_deck))
        .route("/api/editor/image", axum::routing::post(post_editor_image))
        .route("/api/refs", get(get_refs).post(post_ref))
        .route(
            "/api/refs/:id",
            axum::routing::patch(patch_ref).delete(delete_ref),
        )
        .route("/api/images", get(get_images).post(post_image))
        .route(
            "/api/images/:id",
            axum::routing::patch(patch_image).delete(delete_image),
        )
        .route("/api/images/:id/data", get(get_image_data))
        // Serve the active project's deck.html at /slides/deck.html. Resolved
        // per-request (handler, not a static mount) so it always tracks the
        // active project.
        .route("/slides/deck.html", get(get_active_deck_html))
        // Print/PDF layout of the active deck (one 16:9 page per slide).
        // ?print=1 makes it auto-open the print dialog (used by export).
        .route("/slides/deck.pdf.html", get(get_active_deck_pdf))
        // Open the print page in the user's default browser (WKWebView ignores
        // window.print(), so we hand off to a real browser for Save-as-PDF).
        .route("/api/export/open", axum::routing::post(open_export))
        // Open a secondary app window (editor / deck preview). Done server-side
        // because the WKWebView ignores target="_blank", and remote-origin
        // content (our http relay) can't reach Tauri IPC commands.
        .route("/api/open-window", axum::routing::post(open_window))
        .fallback_service(web)
        .with_state(state)
}

/// Open the deck's print page in the default browser (where window.print works
/// and offers "Save as PDF"). We derive our own origin from the request's Host
/// header, so no port plumbing is needed.
async fn open_export(headers: axum::http::HeaderMap) -> impl IntoResponse {
    let host = headers
        .get(axum::http::header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("127.0.0.1");
    let url = format!("http://{host}/slides/deck.pdf.html?print=1");
    match open_in_browser(&url) {
        Ok(()) => (StatusCode::OK, Json(json!({"ok": true}))),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"ok": false, "error": e}))),
    }
}

#[derive(Deserialize)]
struct OpenWindowReq {
    /// Stable window label; reused so a second click focuses the open window.
    label: String,
    /// Relay path to load, e.g. "/editor" or "/slides/deck.html".
    path: String,
    title: String,
}

/// Open (or focus) a secondary app window pointing at one of our own relay
/// pages. Building a window must happen on the main thread, so we hop there via
/// `run_on_main_thread`. We derive the origin from the Host header, same as
/// `open_export`, so no port plumbing is needed.
async fn open_window(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<OpenWindowReq>,
) -> impl IntoResponse {
    use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};

    let Some(app) = state.app.clone() else {
        return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({"ok": false, "error": "no app handle"})));
    };
    let host = headers
        .get(axum::http::header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("127.0.0.1")
        .to_string();
    let url = format!("http://{host}{}", req.path);
    let parsed = match url.parse() {
        Ok(u) => u,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "error": format!("bad url: {e}")})))
        }
    };

    let label = req.label.clone();
    let title = req.title.clone();
    let app_for_closure = app.clone();
    let result = app.run_on_main_thread(move || {
        let app = app_for_closure;
        if let Some(existing) = app.get_webview_window(&label) {
            let _ = existing.set_focus();
            return;
        }
        let mut builder = WebviewWindowBuilder::new(&app, &label, WebviewUrl::External(parsed))
            .title(&title)
            .inner_size(1400.0, 900.0)
            .min_inner_size(800.0, 600.0);
        #[cfg(target_os = "macos")]
        {
            builder = builder
                .title_bar_style(tauri::TitleBarStyle::Overlay)
                .hidden_title(true);
        }
        if let Err(e) = builder.build() {
            log::error!("open_window build failed: {e}");
        }
    });

    match result {
        Ok(()) => (StatusCode::OK, Json(json!({"ok": true}))),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"ok": false, "error": e.to_string()}))),
    }
}

/// Launch the OS default-browser opener for a URL (macOS `open`, Windows
/// `cmd /c start`, Linux `xdg-open`).
fn open_in_browser(url: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let mut cmd = {
        let mut c = std::process::Command::new("open");
        c.arg(url);
        c
    };
    #[cfg(target_os = "windows")]
    let mut cmd = {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "start", "", url]);
        c
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut cmd = {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(url);
        c
    };
    cmd.spawn().map(|_| ()).map_err(|e| format!("open failed: {e}"))
}

// ——— Projects —————————————————————————————————————————————————————————————

async fn get_projects(State(state): State<AppState>) -> impl IntoResponse {
    Json(state.projects().list_json())
}

#[derive(Deserialize)]
struct ProjectPost {
    name: Option<String>,
}

async fn post_project(State(state): State<AppState>, Json(body): Json<ProjectPost>) -> impl IntoResponse {
    match state.projects().create(&body.name.unwrap_or_default()) {
        Ok(id) => (StatusCode::OK, Json(json!({"ok": true, "id": id}))),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"ok": false, "error": e}))),
    }
}

#[derive(Deserialize)]
struct ProjectPatch {
    name: String,
}

async fn patch_project(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<ProjectPatch>,
) -> impl IntoResponse {
    match state.projects().rename(&id, &body.name) {
        Ok(()) => (StatusCode::OK, Json(json!({"ok": true}))),
        Err(e) => (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "error": e}))),
    }
}

async fn delete_project(State(state): State<AppState>, Path(id): Path<String>) -> impl IntoResponse {
    match state.projects().delete(&id) {
        Ok(()) => (StatusCode::OK, Json(json!({"ok": true}))),
        Err(e) => (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "error": e}))),
    }
}

async fn activate_project(State(state): State<AppState>, Path(id): Path<String>) -> impl IntoResponse {
    match state.projects().set_active(&id) {
        Ok(()) => (StatusCode::OK, Json(json!({"ok": true}))),
        Err(e) => (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "error": e}))),
    }
}

/// Serve the active project's rendered deck.html (or a tiny placeholder).
async fn get_active_deck_html(State(state): State<AppState>) -> impl IntoResponse {
    let path = state.project_dir().join("deck.html");
    match tokio::fs::read_to_string(&path).await {
        Ok(html) => ([(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8")], html)
            .into_response(),
        Err(_) => (
            [(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8")],
            "<!doctype html><meta charset=utf-8><body style=\"font-family:system-ui;background:#0b0c10;color:#9aa3b5;display:grid;place-items:center;height:100vh;margin:0\">No deck yet</body>".to_string(),
        )
            .into_response(),
    }
}

/// Serve the active deck laid out for printing (one 16:9 page per slide),
/// rendered fresh from deck.json so it reflects the latest edits.
async fn get_active_deck_pdf(State(state): State<AppState>) -> impl IntoResponse {
    let deck = load_deck(&state);
    (
        [(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8")],
        render_deck_pdf_html(&deck),
    )
}

async fn healthz(State(state): State<AppState>) -> impl IntoResponse {
    let cfg = state.cfg.lock().await;
    Json(json!({
        "status": "ok",
        "configured": cfg.as_ref().map(|c| c.is_complete()).unwrap_or(false),
    }))
}

async fn get_config(State(state): State<AppState>) -> impl IntoResponse {
    let c = state.cfg.lock().await.clone().unwrap_or_default();
    Json(json!({
        "configured": c.is_complete(),
        "provider": c.provider.as_str(),
        "openai_base_url": c.openai_base_url,
        "azure_endpoint": c.azure_endpoint,
        "azure_api_version": c.azure_api_version,
        "image_api_version": c.image_api_version,
        "realtime_model": c.realtime_model,
        "slide_model": c.slide_model,
        "image_model": c.image_model,
        "voice": c.voice,
        "reasoning_effort": c.reasoning_effort,
        "api_key_set": !c.api_key.is_empty(),
    }))
}

#[derive(Deserialize)]
struct ConfigPost {
    provider: Option<String>,
    // Optional on a Settings re-save: blank means "keep what's saved".
    api_key: Option<String>,
    openai_base_url: Option<String>,
    azure_endpoint: Option<String>,
    azure_api_version: Option<String>,
    image_api_version: Option<String>,
    realtime_model: Option<String>,
    slide_model: Option<String>,
    image_model: Option<String>,
    voice: Option<String>,
    reasoning_effort: Option<String>,
}

async fn post_config(
    State(state): State<AppState>,
    Json(body): Json<ConfigPost>,
) -> impl IntoResponse {
    let mut c = state.cfg.lock().await.clone().unwrap_or_default();
    if let Some(v) = body.provider {
        c.provider = Provider::parse(&v);
    }
    if let Some(v) = body.api_key {
        let v = v.trim();
        if !v.is_empty() {
            c.api_key = v.to_string();
        }
    }
    if let Some(v) = body.openai_base_url {
        // Blank -> fall back to the public API default (let the user clear an
        // override without leaving an empty base).
        let v = v.trim().trim_end_matches('/');
        c.openai_base_url = if v.is_empty() {
            "https://api.openai.com/v1".to_string()
        } else {
            v.to_string()
        };
    }
    if let Some(v) = body.azure_endpoint {
        c.azure_endpoint = v.trim().trim_end_matches('/').to_string();
    }
    if let Some(v) = body.azure_api_version {
        c.azure_api_version = v;
    }
    if let Some(v) = body.image_api_version {
        c.image_api_version = v;
    }
    if let Some(v) = body.realtime_model {
        c.realtime_model = v;
    }
    if let Some(v) = body.slide_model {
        c.slide_model = v;
    }
    if let Some(v) = body.image_model {
        c.image_model = v;
    }
    if let Some(v) = body.voice {
        c.voice = v;
    }
    if let Some(v) = body.reasoning_effort {
        c.reasoning_effort = v;
    }

    if let Err(e) = crate::config::save(&c) {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"ok": false, "error": e})));
    }
    let complete = c.is_complete();
    *state.cfg.lock().await = Some(c);
    (StatusCode::OK, Json(json!({"ok": true, "configured": complete})))
}

// ——— Manual editor —————————————————————————————————————————————————————
// The /editor page loads the current deck into a WYSIWYG surface. It reads and
// writes decks_dir/deck.json — the SAME file the AI agent uses — so a manual
// edit is picked up by the next AI build (SlideAgent reloads deck.json before
// each apply) and vice-versa. The file is the hand-off point; we never edit
// manually and via AI at the same instant.

fn deck_json_path(state: &AppState) -> PathBuf {
    state.project_dir().join("deck.json")
}

/// Load the shared deck.json, or an empty deck if none exists yet.
fn load_deck(state: &AppState) -> Deck {
    match std::fs::read_to_string(deck_json_path(state)) {
        Ok(text) => match serde_json::from_str::<Value>(&text) {
            Ok(v) => Deck::from_full_json(&v),
            Err(e) => {
                log::warn!("parse deck.json: {e}");
                Deck::new()
            }
        },
        Err(_) => Deck::new(),
    }
}

/// Persist both the full snapshot (deck.json) and the presentable export
/// (deck.html). Mirrors what SlideAgent does after an AI build.
fn save_deck_full(state: &AppState, deck: &Deck) -> Result<(), String> {
    let dir = state.project_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let json = serde_json::to_string(&deck.to_full_json()).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("deck.json"), json).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("deck.html"), render_deck_html(deck))
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Serve the editor chrome (the page that hosts the WYSIWYG iframe).
async fn get_editor(State(state): State<AppState>) -> impl IntoResponse {
    match tokio::fs::read_to_string(state.web_dir.join("editor.html")).await {
        Ok(html) => ([(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8")], html)
            .into_response(),
        Err(_) => (StatusCode::NOT_FOUND, "editor.html not found").into_response(),
    }
}

/// Return the current deck: outline metadata + the WYSIWYG editor document the
/// iframe loads via srcdoc.
async fn get_deck(State(state): State<AppState>) -> impl IntoResponse {
    let deck = load_deck(&state);
    Json(json!({
        "title": deck.title,
        "slides": deck.slides.iter().map(|s| json!({"id": s.id, "title": s.title})).collect::<Vec<_>>(),
        "editor_html": render_deck_editor_html(&deck),
        "images": deck.images.iter().map(|(id, img)| json!({"id": id, "prompt": img.prompt})).collect::<Vec<_>>(),
    }))
}

#[derive(Deserialize)]
struct DeckPost {
    title: Option<String>,
    slides: Option<Vec<SlideEdit>>,
}

#[derive(Deserialize)]
struct SlideEdit {
    id: String,
    html: Option<String>,
    title: Option<String>,
}

/// Save edits from the /editor. Merges slide html/titles onto the on-disk deck
/// by id (theme + images preserved untouched), then writes deck.json +
/// deck.html.
async fn post_deck(State(state): State<AppState>, Json(body): Json<DeckPost>) -> impl IntoResponse {
    let mut deck = load_deck(&state);

    if let Some(t) = body.title {
        let t = t.trim();
        if !t.is_empty() {
            deck.title = t.to_string();
        }
    }

    let mut updated = 0usize;
    for entry in body.slides.unwrap_or_default() {
        if let Some(idx) = deck.index_of(&entry.id) {
            if let Some(h) = entry.html {
                deck.slides[idx].html = h;
            }
            if let Some(t) = entry.title {
                deck.slides[idx].title = t.trim().to_string();
            }
            updated += 1;
        }
    }

    let total = deck.slides.len();
    match save_deck_full(&state, &deck) {
        Ok(()) => {
            log::info!("editor saved deck ({updated}/{total} slides updated)");
            (StatusCode::OK, Json(json!({"ok": true, "updated": updated, "slides": total})))
        }
        Err(e) => {
            log::warn!("save edited deck: {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"ok": false, "error": e})))
        }
    }
}

#[derive(Deserialize)]
struct EditorImagePost {
    prompt: Option<String>,
    size: Option<String>,
    data_uri: Option<String>,
}

/// Add an image to the deck and return its id + data URI for live preview.
/// Mode A: {prompt, size?} → generate with the image model. Mode B: {data_uri}
/// → store an uploaded image as-is. The image is registered on deck.json under
/// a fresh id; the caller places it on the selected <img> via postMessage, then
/// Save persists the slide html referencing it.
async fn post_editor_image(
    State(state): State<AppState>,
    Json(body): Json<EditorImagePost>,
) -> impl IntoResponse {
    let mut deck = load_deck(&state);

    let prompt = body.prompt.unwrap_or_default().trim().to_string();
    let data_uri = match body.data_uri.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) {
        Some(uri) => uri,
        None => {
            if prompt.is_empty() {
                return (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "error": "need prompt or data_uri"})));
            }
            let cfg = match state.cfg.lock().await.clone() {
                Some(c) if c.is_complete() => c,
                _ => return (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "error": "not configured"}))),
            };
            let size = body.size.unwrap_or_else(|| "1536x1024".to_string());
            match generate_image_data_uri(&cfg, &prompt, &size).await {
                Ok(uri) => uri,
                Err(e) => return (StatusCode::BAD_GATEWAY, Json(json!({"ok": false, "error": e}))),
            }
        }
    };

    let id = deck.new_image_id();
    deck.insert_image(
        id.clone(),
        crate::deck::Image {
            data_uri: data_uri.clone(),
            prompt: if prompt.is_empty() { "uploaded image".to_string() } else { prompt },
        },
    );
    // Persist just deck.json (not deck.html — the slide html that references
    // this image is saved by the subsequent POST /api/deck).
    let json = serde_json::to_string(&deck.to_full_json()).unwrap_or_default();
    if let Err(e) = tokio::fs::write(deck_json_path(&state), json).await {
        log::warn!("persist image to deck.json: {e}");
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"ok": false, "error": e.to_string()})));
    }
    (StatusCode::OK, Json(json!({"ok": true, "image_id": id, "data_uri": data_uri})))
}

/// Call the image model and return a data URI. Mirrors SlideAgent::generate_image.
async fn generate_image_data_uri(cfg: &AppConfig, prompt: &str, size: &str) -> Result<String, String> {
    let url = crate::provider::image_url(cfg)?;
    let mut body = json!({"prompt": prompt, "n": 1, "size": size});
    if let Some(m) = crate::provider::body_model(cfg, &cfg.image_model) {
        body["model"] = json!(m);
    }
    let mut req = reqwest::Client::new().post(&url);
    for (k, v) in crate::provider::auth_headers(cfg) {
        req = req.header(k, v);
    }
    let resp = req.json(&body).send().await.map_err(|e| format!("image request: {e}"))?;
    if !resp.status().is_success() {
        let code = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("image http {code}: {}", truncate(&text, 200)));
    }
    let data: Value = resp.json().await.map_err(|e| format!("image decode: {e}"))?;
    let b64 = data
        .get("data")
        .and_then(|d| d.as_array())
        .and_then(|a| a.first())
        .and_then(|x| x.get("b64_json"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| "no image returned".to_string())?;
    Ok(format!("data:image/png;base64,{b64}"))
}

// ——— Reference material library —————————————————————————————————————————
// User-uploaded .txt/.md files stored under decks_dir/refs/. Enabled files are
// injected into the slide agent's prompt before each build (SlideAgent reads
// the library fresh, so a toggle here takes effect on the next build).

/// GET /api/refs → ordered list of {id, name, enabled, bytes}.
async fn get_refs(State(state): State<AppState>) -> impl IntoResponse {
    let store = RefStore::new(&state.project_dir());
    Json(json!({ "refs": store.list() }))
}

#[derive(Deserialize)]
struct RefPost {
    name: Option<String>,
    content: String,
}

/// POST /api/refs {name, content} → add a text/markdown reference file.
async fn post_ref(State(state): State<AppState>, Json(body): Json<RefPost>) -> impl IntoResponse {
    if body.content.len() > MAX_FILE_BYTES {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(json!({"ok": false, "error": "file too large"})),
        );
    }
    if !looks_textual(&body.name, &body.content) {
        return (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            Json(json!({"ok": false, "error": "only .txt/.md text files are supported"})),
        );
    }
    let store = RefStore::new(&state.project_dir());
    let name = body.name.unwrap_or_else(|| "untitled.txt".to_string());
    match store.add(&name, &body.content) {
        Ok(meta) => (StatusCode::OK, Json(json!({"ok": true, "ref": meta}))),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"ok": false, "error": e}))),
    }
}

#[derive(Deserialize)]
struct RefPatch {
    enabled: bool,
}

/// PATCH /api/refs/:id {enabled} → toggle whether a file is fed to the model.
async fn patch_ref(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<RefPatch>,
) -> impl IntoResponse {
    let store = RefStore::new(&state.project_dir());
    match store.set_enabled(&id, body.enabled) {
        Ok(true) => (StatusCode::OK, Json(json!({"ok": true}))),
        Ok(false) => (StatusCode::NOT_FOUND, Json(json!({"ok": false, "error": "not found"}))),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"ok": false, "error": e}))),
    }
}

/// DELETE /api/refs/:id → remove a file from the library.
async fn delete_ref(State(state): State<AppState>, Path(id): Path<String>) -> impl IntoResponse {
    let store = RefStore::new(&state.project_dir());
    match store.remove(&id) {
        Ok(true) => (StatusCode::OK, Json(json!({"ok": true}))),
        Ok(false) => (StatusCode::NOT_FOUND, Json(json!({"ok": false, "error": "not found"}))),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"ok": false, "error": e}))),
    }
}

/// Accept files that are plausibly plain text: a .txt/.md extension, or content
/// with no NUL bytes (a cheap binary sniff) when the name has no extension.
fn looks_textual(name: &Option<String>, content: &str) -> bool {
    let ext_ok = name
        .as_ref()
        .map(|n| {
            let n = n.to_ascii_lowercase();
            n.ends_with(".txt") || n.ends_with(".md") || n.ends_with(".markdown") || n.ends_with(".text")
        })
        .unwrap_or(false);
    ext_ok || !content.contains('\u{0}')
}

// ——— User image library —————————————————————————————————————————————————
// Pictures the user wants available on slides, each with a text description the
// slide agent reads (and a vision model captions on upload). Stored under
// decks_dir/images/. Enabled images are injected into the deck's image pool
// before each build; the agent places the ones that fit (honoring pins).

/// GET /api/images → metadata only (no base64), so listing stays cheap.
async fn get_images(State(state): State<AppState>) -> impl IntoResponse {
    let store = ImageStore::new(&state.project_dir());
    Json(json!({ "images": store.list() }))
}

/// GET /api/images/:id/data → the raw data URI (for the UI thumbnail).
async fn get_image_data(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let store = ImageStore::new(&state.project_dir());
    match store.read_data_uri(&id) {
        Some(uri) => (StatusCode::OK, Json(json!({"ok": true, "data_uri": uri}))),
        None => (StatusCode::NOT_FOUND, Json(json!({"ok": false, "error": "not found"}))),
    }
}

#[derive(Deserialize)]
struct ImagePost {
    name: Option<String>,
    data_uri: String,
    /// Optional caller-supplied description; if absent we ask a vision model.
    description: Option<String>,
}

/// POST /api/images {name, data_uri, description?} → store an image and, when no
/// description is given, auto-caption it with the (vision-capable) slide model.
async fn post_image(State(state): State<AppState>, Json(body): Json<ImagePost>) -> impl IntoResponse {
    if !body.data_uri.starts_with("data:image/") {
        return (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            Json(json!({"ok": false, "error": "expected an image data: URI"})),
        );
    }
    if body.data_uri.len() > MAX_IMAGE_BYTES {
        return (StatusCode::PAYLOAD_TOO_LARGE, Json(json!({"ok": false, "error": "image too large"})));
    }

    // Description: caller's if provided, else a vision caption (best-effort —
    // a captioning failure still stores the image with an empty description the
    // user can fill in).
    let mut description = body.description.unwrap_or_default().trim().to_string();
    if description.is_empty() {
        if let Some(cfg) = state.cfg.lock().await.clone().filter(|c| c.is_complete()) {
            match caption_image(&cfg, &body.data_uri).await {
                Ok(c) => description = c,
                Err(e) => log::warn!("image caption failed: {e}"),
            }
        }
    }

    let store = ImageStore::new(&state.project_dir());
    let name = body.name.unwrap_or_else(|| "image".to_string());
    match store.add(&name, &body.data_uri, &description) {
        Ok(meta) => (StatusCode::OK, Json(json!({"ok": true, "image": meta}))),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"ok": false, "error": e}))),
    }
}

#[derive(Deserialize)]
struct ImagePatch {
    enabled: Option<bool>,
    description: Option<String>,
    pinned_slide: Option<u32>,
}

/// PATCH /api/images/:id {enabled?, description?, pinned_slide?}.
async fn patch_image(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<ImagePatch>,
) -> impl IntoResponse {
    let store = ImageStore::new(&state.project_dir());
    match store.update(&id, body.enabled, body.description, body.pinned_slide) {
        Ok(true) => (StatusCode::OK, Json(json!({"ok": true}))),
        Ok(false) => (StatusCode::NOT_FOUND, Json(json!({"ok": false, "error": "not found"}))),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"ok": false, "error": e}))),
    }
}

/// DELETE /api/images/:id.
async fn delete_image(State(state): State<AppState>, Path(id): Path<String>) -> impl IntoResponse {
    let store = ImageStore::new(&state.project_dir());
    match store.remove(&id) {
        Ok(true) => (StatusCode::OK, Json(json!({"ok": true}))),
        Ok(false) => (StatusCode::NOT_FOUND, Json(json!({"ok": false, "error": "not found"}))),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"ok": false, "error": e}))),
    }
}

/// Ask the (vision-capable) slide model for a concise description of an image,
/// so the slide agent — which only sees text — knows what it depicts. Uses the
/// chat/completions endpoint with an image_url content part (the same shape for
/// OpenAI and Azure; the data: URI is sent inline).
async fn caption_image(cfg: &AppConfig, data_uri: &str) -> Result<String, String> {
    let url = crate::provider::chat_url(cfg)?;
    let mut body = json!({
        "messages": [{
            "role": "user",
            "content": [
                {"type": "text", "text": "Describe this image for a slide designer in 1–2 sentences. \
State the subject, key visual elements, style, and dominant colors. Be concrete and factual; \
do not add commentary or markdown."},
                {"type": "image_url", "image_url": {"url": data_uri}}
            ]
        }],
        // No temperature: GPT-5-class models reject non-default values (400).
    });
    if let Some(m) = crate::provider::body_model(cfg, &cfg.slide_model) {
        body["model"] = json!(m);
    }
    let mut req = reqwest::Client::new().post(&url);
    for (k, v) in crate::provider::auth_headers(cfg) {
        req = req.header(k, v);
    }
    let resp = req.json(&body).send().await.map_err(|e| format!("caption request: {e}"))?;
    if !resp.status().is_success() {
        let code = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("caption http {code}: {}", truncate(&text, 200)));
    }
    let data: Value = resp.json().await.map_err(|e| format!("caption decode: {e}"))?;
    let text = data
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    if text.is_empty() {
        return Err("empty caption".to_string());
    }
    Ok(text)
}

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| async move {
        let cfg = state.cfg.lock().await.clone();
        let Some(cfg) = cfg else {
            close_with(socket, "config missing").await;
            return;
        };
        if !cfg.is_complete() {
            close_with(socket, "config incomplete").await;
            return;
        }
        // Bind the session to whichever project is active at connect time. The
        // frontend reconnects the socket when the user switches projects, so a
        // fresh session always targets the newly-active project's dir.
        if let Err(e) = run_session(socket, cfg, state.project_dir()).await {
            log::warn!("session ended with error: {e}");
        }
    })
}

async fn close_with(mut socket: WebSocket, reason: &str) {
    let _ = socket
        .send(Message::Text(
            json!({"type": "error", "error": {"code": "config", "message": reason}}).to_string(),
        ))
        .await;
    let _ = socket.close().await;
}

const ALLOWED_CLIENT_EVENTS: &[&str] = &[
    "input_audio_buffer.append",
    "input_audio_buffer.commit",
    "input_audio_buffer.clear",
    "conversation.item.create",
    "response.create",
    "response.cancel",
    "session.update",
];

#[derive(Default)]
struct SessionState {
    response_in_flight: bool,
}

async fn run_session(socket: WebSocket, cfg: AppConfig, project_dir: PathBuf) -> Result<(), String> {
    let (mut browser_sink, mut browser_stream) = socket.split();
    let (browser_tx, mut browser_rx) = mpsc::unbounded_channel::<Value>();

    let browser_writer = tokio::spawn(async move {
        while let Some(v) = browser_rx.recv().await {
            let Ok(payload) = serde_json::to_string(&v) else { continue };
            if browser_sink.send(Message::Text(payload)).await.is_err() {
                break;
            }
        }
        let _ = browser_sink.close().await;
    });

    // Connect upstream realtime (provider-aware url + headers).
    let (ws_url, headers) = crate::provider::realtime_ws(&cfg)?;
    let session_opts = SessionOptions {
        voice: &cfg.voice,
        instructions: BASE_INSTRUCTIONS,
        reasoning_effort: &cfg.reasoning_effort,
    };
    let ws = match realtime::connect(&ws_url, &headers, session_opts.to_session_update()).await {
        Ok(w) => w,
        Err(e) => {
            let _ = browser_tx.send(json!({
                "type": "error",
                "error": {"code": "upstream_connect_failed", "message": e},
            }));
            return Ok(());
        }
    };
    let (azure_tx, mut azure_rx) = realtime::split_ws(ws);

    // Register the build_slides tool + transcription.
    let _ = azure_tx.send(json!({
        "type": "session.update",
        "session": {
            "tools": [build_slides_tool()],
            "tool_choice": "auto",
            "input_audio_transcription": {"model": "whisper-1"},
        }
    }));

    let state = Arc::new(Mutex::new(SessionState::default()));

    // Slide agent: emits deck/status to the browser and persists each render.
    let emit_deck: Emit = {
        let tx = browser_tx.clone();
        Arc::new(move |payload: Value| {
            let mut p = json!({"type": "ui.deck_update"});
            if let (Some(o), Some(src)) = (p.as_object_mut(), payload.as_object()) {
                for (k, v) in src {
                    o.insert(k.clone(), v.clone());
                }
            }
            let _ = tx.send(p);
        })
    };
    let emit_status: Emit = {
        let tx = browser_tx.clone();
        Arc::new(move |payload: Value| {
            let mut p = json!({"type": "ui.slide_status"});
            if let (Some(o), Some(src)) = (p.as_object_mut(), payload.as_object()) {
                for (k, v) in src {
                    o.insert(k.clone(), v.clone());
                }
            }
            let _ = tx.send(p);
        })
    };
    let on_saved: Arc<dyn Fn(String) + Send + Sync> = {
        let dir = project_dir.clone();
        Arc::new(move |html: String| {
            let dir = dir.clone();
            tokio::spawn(async move {
                if let Err(e) = tokio::fs::create_dir_all(&dir).await {
                    log::warn!("mkdir decks: {e}");
                    return;
                }
                if let Err(e) = tokio::fs::write(dir.join("deck.html"), html).await {
                    log::warn!("save deck.html: {e}");
                }
            });
        })
    };
    let agent = Arc::new(SlideAgent::new(
        cfg.clone(),
        emit_deck,
        emit_status,
        on_saved,
        Some(project_dir.join("deck.json")),
        Some(RefStore::new(&project_dir)),
        Some(ImageStore::new(&project_dir)),
    ));

    // Opening greeting.
    {
        let azure_tx = azure_tx.clone();
        let state = state.clone();
        tokio::spawn(async move {
            request_response(
                &azure_tx,
                &state,
                Some("Greet the user in one short, upbeat sentence and invite them to describe the slides they want — for example, how many slides and what each one should cover. Keep it under two sentences.".into()),
            )
            .await;
        });
    }

    // Browser -> azure.
    let browser_to_azure = {
        let azure_tx = azure_tx.clone();
        let state = state.clone();
        tokio::spawn(async move {
            while let Some(msg) = browser_stream.next().await {
                let raw = match msg {
                    Ok(Message::Text(t)) => t,
                    Ok(Message::Close(_)) | Err(_) => break,
                    _ => continue,
                };
                let Ok(evt) = serde_json::from_str::<Value>(&raw) else { continue };
                let etype = evt.get("type").and_then(|v| v.as_str()).unwrap_or("");
                if !ALLOWED_CLIENT_EVENTS.contains(&etype) {
                    continue;
                }
                if etype == "response.create" {
                    let instructions = evt
                        .get("response")
                        .and_then(|r| r.get("instructions"))
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    let _ = request_response(&azure_tx, &state, instructions).await;
                    continue;
                }
                if etype == "response.cancel" {
                    let _ = azure_tx.send(evt);
                    state.lock().await.response_in_flight = false;
                    continue;
                }
                let _ = azure_tx.send(evt);
            }
        })
    };

    // Azure -> browser (intercept build_slides).
    let azure_to_browser = {
        let browser_tx = browser_tx.clone();
        let azure_tx = azure_tx.clone();
        let state = state.clone();
        let agent = agent.clone();
        tokio::spawn(async move {
            while let Some(evt) = azure_rx.recv().await {
                let etype = evt.get("type").and_then(|v| v.as_str()).unwrap_or("").to_string();

                if etype == "response.created" {
                    state.lock().await.response_in_flight = true;
                } else if etype == "response.done" || etype == "response.cancelled" {
                    state.lock().await.response_in_flight = false;
                } else if etype == "response.function_call_arguments.done" {
                    let call_id = evt.get("call_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let name = evt
                        .get("name")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                        .or_else(|| {
                            evt.get("function")
                                .and_then(|f| f.get("name"))
                                .and_then(|n| n.as_str())
                                .map(|s| s.to_string())
                        })
                        .unwrap_or_default();
                    let args_str = evt.get("arguments").and_then(|v| v.as_str()).unwrap_or("{}").to_string();
                    let args: Value = serde_json::from_str(&args_str).unwrap_or(json!({}));
                    let azure_tx = azure_tx.clone();
                    let browser_tx = browser_tx.clone();
                    let state = state.clone();
                    let agent = agent.clone();
                    tokio::spawn(async move {
                        handle_tool_call(&azure_tx, &browser_tx, &state, &agent, call_id, name, args).await;
                    });
                }

                let _ = browser_tx.send(evt);
            }
        })
    };

    let _ = tokio::join!(browser_to_azure, azure_to_browser);
    drop(browser_tx);
    let _ = browser_writer.await;
    Ok(())
}

async fn request_response(
    azure_tx: &mpsc::UnboundedSender<Value>,
    state: &Arc<Mutex<SessionState>>,
    instructions: Option<String>,
) -> bool {
    {
        let mut s = state.lock().await;
        if s.response_in_flight {
            return false;
        }
        s.response_in_flight = true;
    }
    let mut payload = json!({"type": "response.create"});
    if let Some(text) = instructions {
        payload["response"] = json!({"instructions": text});
    }
    azure_tx.send(payload).is_ok()
}

async fn handle_tool_call(
    azure_tx: &mpsc::UnboundedSender<Value>,
    browser_tx: &mpsc::UnboundedSender<Value>,
    state: &Arc<Mutex<SessionState>>,
    agent: &Arc<SlideAgent>,
    call_id: String,
    name: String,
    args: Value,
) {
    if call_id.is_empty() || name != "build_slides" {
        return;
    }
    let instruction = args.get("instruction").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();

    let _ = browser_tx.send(json!({
        "type": "ui.build_pending",
        "call_id": call_id,
        "instruction": truncate(&instruction, 140),
    }));

    // 1) Synchronous ack so the voice agent keeps talking.
    let _ = azure_tx.send(json!({
        "type": "conversation.item.create",
        "item": {
            "type": "function_call_output",
            "call_id": call_id,
            "output": json!({
                "status": "pending",
                "note": "Building the deck now; result arrives as [BUILD RESULT]."
            }).to_string(),
        }
    }));

    // 2) Build (slow).
    let summary = agent.apply(&instruction).await;

    // 3) Inject the result + confirmation turn.
    let _ = browser_tx.send(json!({
        "type": "ui.build_resolved", "call_id": call_id, "summary": summary,
    }));
    let _ = azure_tx.send(json!({
        "type": "conversation.item.create",
        "item": {
            "type": "message",
            "role": "system",
            "content": [{"type": "input_text", "text": format!("[BUILD RESULT] {summary}")}],
        }
    }));
    let _ = request_response(
        azure_tx,
        state,
        Some("Confirm the slide change in one short sentence and invite the next edit. Do not read any markup. Do not repeat the [BUILD RESULT] tag.".into()),
    )
    .await;
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n).collect()
    }
}

fn build_slides_tool() -> Value {
    json!({
        "type": "function",
        "name": "build_slides",
        "description": "Create or modify the slide deck. Call this whenever the user asks to make, add, change, reorder, restyle, or fix slides. Pass the user's full intent in plain language — the slide designer that receives it writes the actual HTML/CSS, so be specific and complete. Returns immediately with a pending acknowledgement; the finished deck arrives shortly as a [BUILD RESULT] message and is shown on screen.",
        "parameters": {
            "type": "object",
            "properties": {
                "instruction": {
                    "type": "string",
                    "description": "The complete instruction for the slide designer, in plain language. Include slide counts, per-slide topics, and any style direction the user gave. Preserve the user's specifics."
                }
            },
            "required": ["instruction"]
        }
    })
}

pub const BASE_INSTRUCTIONS: &str = "You are a friendly, fast voice assistant that helps the user build an HTML slide deck by talking. Speak concisely and naturally. You do NOT write HTML or CSS yourself — a separate slide designer does that. Your job is to understand what the user wants and call the build_slides tool with a clear, complete instruction in plain language, preserving their specifics (how many slides, what each slide is about, any style direction). When the user describes multiple slides at once, capture ALL of them in a single build_slides call. When they ask to tweak one slide or restyle the whole deck, pass that as the instruction too. The tool returns a pending acknowledgement immediately — tell the user briefly what you're building (one short sentence), then stop and wait. When a [BUILD RESULT] message arrives, confirm what changed in one short sentence and invite the next change. Do not read slide markup aloud. If the user is just chatting or asking what you can do, answer briefly and suggest they describe the slides they want.";
