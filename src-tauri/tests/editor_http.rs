//! End-to-end HTTP test for the manual editor endpoints. Boots the real axum
//! router against a temp decks dir and exercises the non-AI editor flow:
//! seed deck.json → GET /api/deck → POST /api/deck (edit) → verify deck.json +
//! deck.html on disk, and an uploaded-image round-trip. The AI image-generation
//! branch is not exercised (needs a live key); the data_uri upload branch is.

use std::sync::Arc;

use app_lib::test_support::{build_router, AppState};
use serde_json::{json, Value};
use tokio::sync::Mutex;

async fn spawn(decks: std::path::PathBuf, web: std::path::PathBuf) -> String {
    let state = AppState {
        web_dir: web,
        decks_dir: decks,
        cfg: Arc::new(Mutex::new(Some(Default::default()))),
        app: None,
    };
    let router = build_router(state).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    format!("http://{addr}")
}

/// Per-deck files now live in the active project's dir. On a fresh decks dir
/// that's `<decks>/projects/default/` (created on first access, with any legacy
/// global deck migrated into it).
fn proj_dir(decks: &std::path::Path) -> std::path::PathBuf {
    decks.join("projects").join("default")
}

#[tokio::test]
async fn editor_round_trip() {
    let tmp = std::env::temp_dir().join(format!("sc-editor-test-{}", std::process::id()));
    let decks = tmp.join("decks");
    let web = tmp.join("web");
    std::fs::create_dir_all(&decks).unwrap();
    std::fs::create_dir_all(&web).unwrap();
    std::fs::write(web.join("editor.html"), "<!doctype html><title>ed</title>").unwrap();

    // Seed a deck.json as if a prior AI build (or manual edit) wrote it.
    let seed = json!({
        "title": "Demo",
        "theme_css": "body{background:#101}",
        "slides": [
            {"id": "s1", "title": "Title", "html": "<h1 class=\"slide-title\">Original</h1>"},
            {"id": "s2", "title": "Two", "html": "<ul class=\"bullets\"><li>a</li></ul>"}
        ],
        "images": [],
        "_seq": 2, "_img_seq": 0
    });
    std::fs::write(decks.join("deck.json"), serde_json::to_string(&seed).unwrap()).unwrap();

    let base = spawn(decks.clone(), web.clone()).await;
    let http = reqwest::Client::new();

    // GET /editor serves the chrome.
    let r = http.get(format!("{base}/editor")).send().await.unwrap();
    assert!(r.status().is_success());

    // GET /api/deck returns metadata + a WYSIWYG doc with the editor runtime.
    let deck: Value = http.get(format!("{base}/api/deck")).send().await.unwrap().json().await.unwrap();
    assert_eq!(deck["title"], "Demo");
    assert_eq!(deck["slides"].as_array().unwrap().len(), 2);
    let editor_html = deck["editor_html"].as_str().unwrap();
    assert!(editor_html.contains("ed-serialize"));
    assert!(editor_html.contains("Original"));

    // POST /api/deck edits slide 1 (with an inline style) and leaves slide 2.
    let save: Value = http
        .post(format!("{base}/api/deck"))
        .json(&json!({
            "title": "Demo",
            "slides": [{"id": "s1", "html": "<h1 class=\"slide-title\" style=\"color: rgb(255,0,0);\">EDITED</h1>"}]
        }))
        .send().await.unwrap().json().await.unwrap();
    assert_eq!(save["ok"], true);
    assert_eq!(save["updated"], 1);

    // deck.json updated; slide 2 untouched. (Lives in the project dir now —
    // the seed written to the legacy path was migrated there on first access.)
    let pdir = proj_dir(&decks);
    let on_disk: Value =
        serde_json::from_str(&std::fs::read_to_string(pdir.join("deck.json")).unwrap()).unwrap();
    assert!(on_disk["slides"][0]["html"].as_str().unwrap().contains("EDITED"));
    assert!(on_disk["slides"][1]["html"].as_str().unwrap().contains("<li>a</li>"));

    // deck.html written, presentable (nav present, no editor runtime).
    let html = std::fs::read_to_string(pdir.join("deck.html")).unwrap();
    assert!(html.contains("EDITED"));
    assert!(html.contains("deck-nav"));
    assert!(!html.contains("ed-serialize"));

    // Uploaded-image branch: store a data_uri, get back an id, and confirm it
    // landed in deck.json's images.
    let img: Value = http
        .post(format!("{base}/api/editor/image"))
        .json(&json!({"data_uri": "data:image/png;base64,ZZZZ"}))
        .send().await.unwrap().json().await.unwrap();
    assert_eq!(img["ok"], true);
    let iid = img["image_id"].as_str().unwrap();
    assert_eq!(iid, "img1");
    let on_disk2: Value =
        serde_json::from_str(&std::fs::read_to_string(pdir.join("deck.json")).unwrap()).unwrap();
    let images = on_disk2["images"].as_array().unwrap();
    assert_eq!(images.len(), 1);
    assert_eq!(images[0]["data_uri"], "data:image/png;base64,ZZZZ");

    let _ = std::fs::remove_dir_all(&tmp);
}

#[tokio::test]
async fn projects_isolated() {
    let tmp = std::env::temp_dir().join(format!("sc-proj-test-{}", std::process::id()));
    let decks = tmp.join("decks");
    let web = tmp.join("web");
    std::fs::create_dir_all(&decks).unwrap();
    std::fs::create_dir_all(&web).unwrap();

    let base = spawn(decks.clone(), web.clone()).await;
    let http = reqwest::Client::new();

    // Boot creates a single "Default" project, active.
    let list: Value = http.get(format!("{base}/api/projects")).send().await.unwrap().json().await.unwrap();
    assert_eq!(list["projects"].as_array().unwrap().len(), 1);
    let default_id = list["active"].as_str().unwrap().to_string();

    // Save a deck in the default project.
    http.post(format!("{base}/api/deck"))
        .json(&json!({"title": "Alpha Deck", "slides": []}))
        .send().await.unwrap();

    // Create a second project (auto-activated).
    let created: Value = http.post(format!("{base}/api/projects"))
        .json(&json!({"name": "Beta"})).send().await.unwrap().json().await.unwrap();
    assert_eq!(created["ok"], true);
    let beta_id = created["id"].as_str().unwrap().to_string();

    // The new project starts empty — NOT showing Alpha's title.
    let deck_b: Value = http.get(format!("{base}/api/deck")).send().await.unwrap().json().await.unwrap();
    assert_ne!(deck_b["title"], "Alpha Deck");

    // Save a distinct deck in Beta.
    http.post(format!("{base}/api/deck"))
        .json(&json!({"title": "Beta Deck", "slides": []}))
        .send().await.unwrap();

    // Switch back to default → Alpha's deck is intact (isolation holds).
    http.post(format!("{base}/api/projects/{default_id}/activate")).send().await.unwrap();
    let deck_a: Value = http.get(format!("{base}/api/deck")).send().await.unwrap().json().await.unwrap();
    assert_eq!(deck_a["title"], "Alpha Deck");

    // Each project has its own dir on disk.
    assert!(decks.join("projects").join(&default_id).join("deck.json").is_file());
    assert!(decks.join("projects").join(&beta_id).join("deck.json").is_file());

    let _ = std::fs::remove_dir_all(&tmp);
}

#[tokio::test]
async fn refs_round_trip() {
    let tmp = std::env::temp_dir().join(format!("sc-refs-http-{}", std::process::id()));
    let decks = tmp.join("decks");
    let web = tmp.join("web");
    std::fs::create_dir_all(&decks).unwrap();
    std::fs::create_dir_all(&web).unwrap();

    let base = spawn(decks.clone(), web.clone()).await;
    let http = reqwest::Client::new();

    // Empty to start.
    let list: Value = http.get(format!("{base}/api/refs")).send().await.unwrap().json().await.unwrap();
    assert_eq!(list["refs"].as_array().unwrap().len(), 0);

    // Upload a markdown file.
    let up: Value = http
        .post(format!("{base}/api/refs"))
        .json(&json!({"name": "brief.md", "content": "# Q3 Brief\nRevenue up 20%."}))
        .send().await.unwrap().json().await.unwrap();
    assert_eq!(up["ok"], true);
    let id = up["ref"]["id"].as_str().unwrap().to_string();
    assert_eq!(up["ref"]["enabled"], true);

    // A binary-ish .png name with NUL content is rejected.
    let bad = http
        .post(format!("{base}/api/refs"))
        .json(&json!({"name": "x.png", "content": "\u{0}\u{0}binary"}))
        .send().await.unwrap();
    assert_eq!(bad.status(), reqwest::StatusCode::UNSUPPORTED_MEDIA_TYPE);

    // Enabled file appears in the manifest (id + name + preview) and its full
    // text is fetchable via read() — the manifest+read flow the agent uses.
    let store = app_lib::test_support::RefStore::new(&proj_dir(&decks));
    let manifest = store.manifest_block().unwrap();
    assert!(manifest.contains("brief.md"));
    assert!(manifest.contains(&format!("id \"{id}\"")));
    assert_eq!(store.read(&id).as_deref(), Some("# Q3 Brief\nRevenue up 20%."));

    // Toggle off → gone from manifest AND unreadable.
    let patch: Value = http
        .patch(format!("{base}/api/refs/{id}"))
        .json(&json!({"enabled": false}))
        .send().await.unwrap().json().await.unwrap();
    assert_eq!(patch["ok"], true);
    assert!(store.manifest_block().is_none());
    assert!(store.read(&id).is_none());

    // Delete.
    let del: Value = http.delete(format!("{base}/api/refs/{id}")).send().await.unwrap().json().await.unwrap();
    assert_eq!(del["ok"], true);
    let list2: Value = http.get(format!("{base}/api/refs")).send().await.unwrap().json().await.unwrap();
    assert_eq!(list2["refs"].as_array().unwrap().len(), 0);

    let _ = std::fs::remove_dir_all(&tmp);
}

#[tokio::test]
async fn images_round_trip() {
    let tmp = std::env::temp_dir().join(format!("sc-img-http-{}", std::process::id()));
    let decks = tmp.join("decks");
    let web = tmp.join("web");
    std::fs::create_dir_all(&decks).unwrap();
    std::fs::create_dir_all(&web).unwrap();

    let base = spawn(decks.clone(), web.clone()).await;
    let http = reqwest::Client::new();

    // Upload with an explicit description (skips the vision call; the test
    // config isn't complete enough to caption anyway).
    let up: Value = http
        .post(format!("{base}/api/images"))
        .json(&json!({
            "name": "team.png",
            "data_uri": "data:image/png;base64,AAAA",
            "description": "the engineering team on a rooftop"
        }))
        .send().await.unwrap().json().await.unwrap();
    assert_eq!(up["ok"], true);
    let id = up["image"]["id"].as_str().unwrap().to_string();
    assert_eq!(id, "lib1");
    assert_eq!(up["image"]["description"], "the engineering team on a rooftop");
    assert_eq!(up["image"]["enabled"], true);
    assert_eq!(up["image"]["pinned_slide"], 0);

    // Non-image data URI rejected.
    let bad = http
        .post(format!("{base}/api/images"))
        .json(&json!({"data_uri": "data:text/plain;base64,AAAA"}))
        .send().await.unwrap();
    assert_eq!(bad.status(), reqwest::StatusCode::UNSUPPORTED_MEDIA_TYPE);

    // List returns metadata (no base64).
    let list: Value = http.get(format!("{base}/api/images")).send().await.unwrap().json().await.unwrap();
    assert_eq!(list["images"].as_array().unwrap().len(), 1);
    assert!(list["images"][0].get("data_uri").is_none());

    // Data endpoint returns the URI for the thumbnail.
    let data: Value = http.get(format!("{base}/api/images/{id}/data")).send().await.unwrap().json().await.unwrap();
    assert_eq!(data["data_uri"], "data:image/png;base64,AAAA");

    // Patch: pin to slide 2 + edit description.
    let patch: Value = http
        .patch(format!("{base}/api/images/{id}"))
        .json(&json!({"pinned_slide": 2, "description": "rooftop team photo, golden hour"}))
        .send().await.unwrap().json().await.unwrap();
    assert_eq!(patch["ok"], true);

    // The enabled image, with its description + pin, reaches the injection path.
    let store = app_lib::test_support::ImageStore::new(&proj_dir(&decks));
    let en = store.enabled_images();
    assert_eq!(en.len(), 1);
    assert_eq!(en[0].meta.pinned_slide, 2);
    assert_eq!(en[0].meta.description, "rooftop team photo, golden hour");
    assert_eq!(en[0].data_uri, "data:image/png;base64,AAAA");

    // Disable → excluded from builds.
    http.patch(format!("{base}/api/images/{id}"))
        .json(&json!({"enabled": false}))
        .send().await.unwrap();
    assert!(store.enabled_images().is_empty());

    // Delete.
    let del: Value = http.delete(format!("{base}/api/images/{id}")).send().await.unwrap().json().await.unwrap();
    assert_eq!(del["ok"], true);
    let list2: Value = http.get(format!("{base}/api/images")).send().await.unwrap().json().await.unwrap();
    assert_eq!(list2["images"].as_array().unwrap().len(), 0);

    let _ = std::fs::remove_dir_all(&tmp);
}
