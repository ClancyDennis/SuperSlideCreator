//! Slide-building agent (Rust port of slide_agent.py).
//!
//! Holds the authoritative deck and applies a natural-language instruction via
//! granular, id-based tools (write_deck / add_slide / edit_slide /
//! delete_slide / move_slide / set_theme / set_title / generate_image). The
//! model may batch several tool calls per turn; image generation in a turn runs
//! concurrently. Mutations apply to a working copy, then the deck is rendered,
//! saved, and pushed to the browser once.

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{json, Value};
use tokio::sync::Mutex;

use crate::config::AppConfig;
use crate::deck::{render_deck_html, Deck, Image, Slide};
use crate::images::ImageStore;
use crate::provider;
use crate::refs::RefStore;

const MAX_TOOL_ROUNDS: usize = 4;
const MAX_OPS_PER_INSTRUCTION: usize = 40;
/// Max reference files one build may read via read_reference. Keeps the
/// manifest+fetch design honest — the model can't just pull the whole library.
const MAX_REFERENCE_READS: usize = 12;
/// Cap on a single returned reference file's text, so one huge file can't blow
/// the context even when deliberately read.
const MAX_REFERENCE_READ_BYTES: usize = 120_000;

/// Callback that pushes a JSON event to the browser websocket.
pub type Emit = Arc<dyn Fn(Value) + Send + Sync>;

pub struct SlideAgent {
    cfg: AppConfig,
    http: reqwest::Client,
    emit_deck: Emit,
    emit_status: Emit,
    on_saved: Arc<dyn Fn(String) + Send + Sync>,
    deck: Mutex<Deck>,
    lock: Mutex<()>,
    run_count: std::sync::atomic::AtomicU32,
    /// Path to the shared deck.json source of truth. The agent reloads it before
    /// every edit so manual edits made in the /editor (which writes this file)
    /// are visible to the model — and writes it back after every edit.
    deck_path: Option<PathBuf>,
    /// Reference-material library. Enabled files are injected into the prompt
    /// (read fresh before each build, so UI toggles take effect immediately).
    refs: Option<RefStore>,
    /// User image library. Enabled images are injected into the deck's image
    /// pool before each build (so the model can place them by id), with their
    /// descriptions + any slide pins surfaced to the model. Read fresh each
    /// build so UI changes take effect immediately.
    images: Option<ImageStore>,
}

impl SlideAgent {
    pub fn new(
        cfg: AppConfig,
        emit_deck: Emit,
        emit_status: Emit,
        on_saved: Arc<dyn Fn(String) + Send + Sync>,
        deck_path: Option<PathBuf>,
        refs: Option<RefStore>,
        images: Option<ImageStore>,
    ) -> Self {
        // Adopt any deck already on disk (from a prior session or manual edit).
        let initial = deck_path.as_ref().and_then(load_deck_file).unwrap_or_else(Deck::new);
        SlideAgent {
            cfg,
            http: reqwest::Client::new(),
            emit_deck,
            emit_status,
            on_saved,
            deck: Mutex::new(initial),
            lock: Mutex::new(()),
            run_count: std::sync::atomic::AtomicU32::new(0),
            deck_path,
            refs,
            images,
        }
    }

    /// Persist the full deck snapshot to deck.json (the shared source of truth).
    fn persist_to_disk(&self, deck: &Deck) {
        let Some(path) = self.deck_path.clone() else { return };
        let json = serde_json::to_string(&deck.to_full_json()).unwrap_or_default();
        tokio::spawn(async move {
            if let Some(dir) = path.parent() {
                let _ = tokio::fs::create_dir_all(dir).await;
            }
            if let Err(e) = tokio::fs::write(&path, json).await {
                log::warn!("persist deck.json: {e}");
            }
        });
    }

    /// Upsert every enabled library image into the deck's image pool (id = the
    /// library id, e.g. `lib3`; prompt = its description so the model knows what
    /// it shows) and return a `[USER IMAGES]` directive block describing them.
    /// Returns "" when there are none. The base64 lives only in the pool, never
    /// in this block, so the model context stays small.
    fn inject_library_images(&self, working: &mut Deck) -> String {
        let Some(store) = self.images.as_ref() else { return String::new() };
        let enabled = store.enabled_images();
        if enabled.is_empty() {
            return String::new();
        }
        let mut lines = String::from(
            "[USER IMAGES — real images the user has provided for this deck. Each has a stable \
id; place one on a slide with <img class=\"slide-image\" data-img=\"<id>\" alt=\"…\"> (no src — \
it is filled in automatically), laying it out per the IMAGES guidance. Use the ones that fit \
the content and the user's instruction; you need not use every one. Do NOT call generate_image \
for something a user image already covers.]\n",
        );
        for img in &enabled {
            let desc = if img.meta.description.is_empty() {
                "(no description provided)"
            } else {
                img.meta.description.as_str()
            };
            let pin = if img.meta.pinned_slide > 0 {
                format!(" — REQUESTED on slide {}", img.meta.pinned_slide)
            } else {
                String::new()
            };
            lines.push_str(&format!(
                "- id \"{}\" ({}): {}{}\n",
                img.meta.id, img.meta.name, desc, pin
            ));
            working.set_image(
                img.meta.id.clone(),
                Image {
                    data_uri: img.data_uri.clone(),
                    prompt: if img.meta.description.is_empty() {
                        img.meta.name.clone()
                    } else {
                        img.meta.description.clone()
                    },
                },
            );
        }
        lines.push('\n');
        lines
    }

    /// Drop injected library images (`lib*` ids) that the build left unplaced,
    /// so deck.json doesn't accumulate every enabled image whether used or not.
    fn prune_unused_library_images(&self, working: &mut Deck) {
        let referenced = working.referenced_image_ids();
        let unused: std::collections::HashSet<String> = working
            .images
            .iter()
            .filter(|(id, _)| id.starts_with("lib") && !referenced.contains(id))
            .map(|(id, _)| id.clone())
            .collect();
        if !unused.is_empty() {
            working.retain_images_not_in(&unused);
        }
    }

    /// Serve a read_reference tool call: return the requested file's full text
    /// (capped), decrementing the per-build read budget. Read-only.
    fn handle_read_reference(&self, args: &Value, reads_left: &mut usize) -> Value {
        let id = sget(args, "id");
        let id = id.trim();
        if id.is_empty() {
            return json!({"ok": false, "error": "missing reference id"});
        }
        if *reads_left == 0 {
            return json!({"ok": false, "error": "reference read limit reached for this build"});
        }
        let Some(store) = self.refs.as_ref() else {
            return json!({"ok": false, "error": "no reference library"});
        };
        match store.read(id) {
            Some(content) => {
                *reads_left -= 1;
                let (content, truncated) = if content.len() > MAX_REFERENCE_READ_BYTES {
                    let mut end = MAX_REFERENCE_READ_BYTES;
                    while end > 0 && !content.is_char_boundary(end) {
                        end -= 1;
                    }
                    (content[..end].to_string(), true)
                } else {
                    (content, false)
                };
                self.status("reading", &format!("reference {id}"));
                json!({"ok": true, "id": id, "content": content, "truncated": truncated})
            }
            None => json!({"ok": false, "error": format!("no enabled reference with id {id:?}")}),
        }
    }

    fn status(&self, state: &str, note: &str) {
        let run = self.run_count.load(std::sync::atomic::Ordering::Relaxed);
        (self.emit_status)(json!({"state": state, "note": note, "run_count": run}));
    }

    /// Apply one instruction. Returns a short status line for the voice agent.
    pub async fn apply(&self, instruction: &str) -> String {
        let instruction = instruction.trim();
        if instruction.is_empty() {
            return "No instruction given.".to_string();
        }
        let _guard = self.lock.lock().await;
        self.run_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.status("building", &truncate(instruction, 80));

        // Pick up any manual edits made in the /editor since the last build, so
        // the model edits the deck the user is actually looking at.
        if let Some(disk) = self.deck_path.as_ref().and_then(load_deck_file) {
            *self.deck.lock().await = disk;
        }

        // Work on a copy so a mid-way failure can't half-apply.
        let mut working = { self.deck.lock().await.clone() };

        let applied = match self.run_tool_loop(&mut working, instruction).await {
            Ok(a) => a,
            Err(e) => {
                // Surface the real cause in the app log — without this, the UI
                // shows "build error" while the logs stay silent.
                log::error!("slide build failed: {e}");
                self.status("error", &e);
                return format!("Slide edit failed: {e}.");
            }
        };

        if applied.is_empty() {
            self.status("ready", "no change");
            return "I didn't change anything — could you rephrase?".to_string();
        }

        working.ensure_ids();
        // Drop any enabled library images the model chose not to place, so they
        // don't pile up in deck.json build after build.
        self.prune_unused_library_images(&mut working);
        let html = render_deck_html(&working);
        let summary = summarize(&applied, &working);

        // Persist the shared source of truth first so the /editor and a restart
        // both see this build.
        self.persist_to_disk(&working);
        (self.emit_deck)(json!({"deck": working.to_ui_json(), "html": html}));
        (self.on_saved)(render_deck_html(&working));
        *self.deck.lock().await = working;

        self.status("ready", &summary);
        summary
    }

    async fn run_tool_loop(&self, working: &mut Deck, instruction: &str) -> Result<Vec<String>, String> {
        // Offer enabled reference files as a lightweight MANIFEST (read fresh so
        // UI toggles take effect on the next build). The model reads the full
        // text of only the files it wants via the read_reference tool, so a big
        // library doesn't blow the context. `has_refs` gates whether we even
        // advertise that tool.
        let refs_block = self
            .refs
            .as_ref()
            .and_then(|r| r.manifest_block())
            .map(|b| format!("{b}\n\n"))
            .unwrap_or_default();
        let has_refs = !refs_block.is_empty();
        let tool_list = tools(has_refs);
        // Bound how many files one build may pull, so the model can't defeat the
        // point of the manifest by reading the whole library anyway.
        let mut reads_left: usize = MAX_REFERENCE_READS;

        // Inject enabled user-library images into the deck's image pool so the
        // model can place them by id (exactly like generated images), and tell
        // it what each one shows + any slide it's pinned to. Read fresh so UI
        // changes take effect on the next build. Unplaced ones are pruned after.
        let images_block = self.inject_library_images(working);

        let mut messages = vec![
            json!({"role": "system", "content": SYSTEM_PROMPT}),
            json!({"role": "user", "content": format!(
                "[CURRENT DECK — JSON, slides carry stable ids]\n{}\n\n{}{}[INSTRUCTION]\n{}\n\nApply it with the smallest set of tool calls. Reference slides by id.",
                working.to_model_json(),
                refs_block,
                images_block,
                instruction,
            )}),
        ];
        let mut applied: Vec<String> = Vec::new();

        for _round in 0..MAX_TOOL_ROUNDS {
            let msg = self.chat(&messages, &tool_list).await?;
            messages.push(msg.clone());
            let tool_calls = msg.get("tool_calls").and_then(|v| v.as_array()).cloned().unwrap_or_default();
            if tool_calls.is_empty() {
                break;
            }

            // Parse calls.
            let mut parsed: Vec<(String, String, Value)> = Vec::new(); // (call_id, name, args)
            for call in &tool_calls {
                let id = call.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let f = call.get("function").cloned().unwrap_or(json!({}));
                let name = f.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let args_str = f.get("arguments").and_then(|v| v.as_str()).unwrap_or("{}");
                let args: Value = serde_json::from_str(args_str).unwrap_or(json!({}));
                parsed.push((id, name, args));
            }

            // Run all generate_image calls in this batch concurrently.
            let img_indices: Vec<usize> = parsed
                .iter()
                .enumerate()
                .filter(|(_, (_, n, _))| n == "generate_image")
                .map(|(i, _)| i)
                .collect();
            // idx -> Ok(Image) on success, Err(error-json) on failure.
            let mut img_results: std::collections::HashMap<usize, Result<Image, Value>> =
                std::collections::HashMap::new();
            if !img_indices.is_empty() {
                self.status("imaging", &format!("{} image(s)", img_indices.len()));
                let futs = img_indices.iter().map(|&idx| {
                    let prompt = parsed[idx].2.get("prompt").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let size = parsed[idx].2.get("size").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    async move { (idx, self.generate_image(&prompt, &size).await) }
                });
                let results = futures_util::future::join_all(futs).await;
                for (idx, res) in results {
                    img_results.insert(idx, res);
                }
            }

            // Apply in call order, then report each tool result back to the model.
            // Image ids are minted here (under the apply lock) so they stay
            // sequential (img1, img2, …) and never collide.
            for (i, (call_id, name, args)) in parsed.iter().enumerate() {
                let result: Value = if name == "read_reference" {
                    // Read-only: doesn't mutate the deck, doesn't count toward
                    // the op limit. Bounded by reads_left so the model can't
                    // siphon the whole library in one build.
                    self.handle_read_reference(args, &mut reads_left)
                } else if applied.len() >= MAX_OPS_PER_INSTRUCTION {
                    json!({"ok": false, "error": "op limit reached"})
                } else if name == "generate_image" {
                    match img_results.remove(&i) {
                        Some(Ok(img)) => {
                            let id = working.new_image_id();
                            let note = format!(
                                "Use it on a slide with <img class=\"slide-image\" data-img=\"{id}\">."
                            );
                            working.insert_image(id.clone(), img);
                            json!({"ok": true, "image_id": id, "desc": "generated an image", "note": note})
                        }
                        Some(Err(err_json)) => err_json,
                        None => json!({"ok": false, "error": "image task lost"}),
                    }
                } else {
                    self.dispatch(working, name, args)
                };
                if result.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
                    if let Some(d) = result.get("desc").and_then(|v| v.as_str()) {
                        applied.push(d.to_string());
                    }
                }
                messages.push(json!({
                    "role": "tool",
                    "tool_call_id": call_id,
                    "content": result.to_string(),
                }));
            }
        }
        Ok(applied)
    }

    // ——— image generation ———
    // Returns the decoded Image on success; the caller mints the id and inserts
    // it under the apply lock (so ids stay sequential). Err carries the
    // tool-result JSON to feed back to the model.
    async fn generate_image(&self, prompt: &str, size: &str) -> Result<Image, Value> {
        let prompt = prompt.trim();
        if prompt.is_empty() {
            return Err(json!({"ok": false, "error": "empty prompt"}));
        }
        let size = if size.is_empty() { "1536x1024" } else { size };
        let url = provider::image_url(&self.cfg).map_err(|e| json!({"ok": false, "error": e}))?;
        let mut body = json!({"prompt": prompt, "n": 1, "size": size});
        if let Some(m) = provider::body_model(&self.cfg, &self.cfg.image_model) {
            body["model"] = json!(m);
        }
        let mut req = self.http.post(&url);
        for (k, v) in provider::auth_headers(&self.cfg) {
            req = req.header(k, v);
        }
        let resp = req
            .json(&body)
            .send()
            .await
            .map_err(|e| json!({"ok": false, "error": format!("image request: {e}")}))?;
        if !resp.status().is_success() {
            let code = resp.status().as_u16();
            let text = resp.text().await.unwrap_or_default();
            return Err(json!({"ok": false, "error": format!("image http {code}: {}", truncate(&text, 200))}));
        }
        let data: Value = resp
            .json()
            .await
            .map_err(|e| json!({"ok": false, "error": format!("image decode: {e}")}))?;
        let b64 = data
            .get("data")
            .and_then(|d| d.as_array())
            .and_then(|a| a.first())
            .and_then(|x| x.get("b64_json"))
            .and_then(|v| v.as_str());
        let Some(b64) = b64 else {
            return Err(json!({"ok": false, "error": "no image returned"}));
        };
        Ok(Image {
            data_uri: format!("data:image/png;base64,{b64}"),
            prompt: prompt.to_string(),
        })
    }

    // ——— sync ops on the working deck ———
    fn dispatch(&self, deck: &mut Deck, name: &str, args: &Value) -> Value {
        match name {
            "write_deck" => self.op_write_deck(deck, args),
            "add_slide" => self.op_add_slide(deck, args),
            "edit_slide" => self.op_edit_slide(deck, args),
            "delete_slide" => self.op_delete_slide(deck, args),
            "move_slide" => self.op_move_slide(deck, args),
            "set_theme" => self.op_set_theme(deck, args),
            "set_title" => self.op_set_title(deck, args),
            other => json!({"ok": false, "error": format!("unknown tool: {other}")}),
        }
    }

    fn op_write_deck(&self, deck: &mut Deck, args: &Value) -> Value {
        deck.theme_css = sget(args, "theme_css");
        deck.slides.clear();
        if let Some(arr) = args.get("slides").and_then(|v| v.as_array()) {
            for s in arr {
                let id = deck.new_id();
                deck.slides.push(Slide {
                    id,
                    title: sget(s, "title").trim().to_string(),
                    html: sget(s, "html"),
                });
            }
        }
        let mut title = sget(args, "title").trim().to_string();
        if title.is_empty() {
            if let Some(first) = deck.slides.first() {
                title = first.title.clone();
            }
        }
        deck.title = if title.is_empty() { "Untitled Deck".into() } else { title };
        json!({"ok": true, "desc": format!("rebuilt deck ({} slides)", deck.slides.len())})
    }

    fn op_add_slide(&self, deck: &mut Deck, args: &Value) -> Value {
        let id = deck.new_id();
        let title = sget(args, "title").trim().to_string();
        let slide = Slide { id: id.clone(), title: title.clone(), html: sget(args, "html") };
        let after = sget(args, "after_id");
        let after = after.trim();
        if !after.is_empty() {
            match deck.index_of(after) {
                Some(idx) => deck.slides.insert(idx + 1, slide),
                None => deck.slides.push(slide),
            }
        } else {
            deck.slides.push(slide);
        }
        if (deck.title.is_empty() || deck.title == "Untitled Deck") && !title.is_empty() {
            deck.title = title.clone();
        }
        json!({"ok": true, "desc": format!("added slide \u{201c}{title}\u{201d}"), "id": id})
    }

    fn op_edit_slide(&self, deck: &mut Deck, args: &Value) -> Value {
        let sid = sget(args, "id");
        let sid = sid.trim();
        let Some(idx) = deck.index_of(sid) else {
            return json!({"ok": false, "error": format!("no slide with id {sid:?}")});
        };
        let mut changed: Vec<&str> = Vec::new();
        if let Some(t) = args.get("title").and_then(|v| v.as_str()) {
            deck.slides[idx].title = t.trim().to_string();
            changed.push("title");
        }
        if let Some(h) = args.get("html").and_then(|v| v.as_str()) {
            deck.slides[idx].html = h.to_string();
            changed.push("content");
        }
        if changed.is_empty() {
            return json!({"ok": false, "error": "nothing to change"});
        }
        json!({"ok": true, "desc": format!("edited slide {} ({})", idx + 1, changed.join("/"))})
    }

    fn op_delete_slide(&self, deck: &mut Deck, args: &Value) -> Value {
        let sid = sget(args, "id");
        let sid = sid.trim();
        let Some(idx) = deck.index_of(sid) else {
            return json!({"ok": false, "error": format!("no slide with id {sid:?}")});
        };
        let s = deck.slides.remove(idx);
        json!({"ok": true, "desc": format!("deleted slide \u{201c}{}\u{201d}", s.title)})
    }

    fn op_move_slide(&self, deck: &mut Deck, args: &Value) -> Value {
        let sid = sget(args, "id");
        let sid = sid.trim();
        let Some(idx) = deck.index_of(sid) else {
            return json!({"ok": false, "error": format!("no slide with id {sid:?}")});
        };
        let mut to = args.get("to_index").and_then(|v| v.as_i64()).unwrap_or(idx as i64);
        let last = deck.slides.len() as i64 - 1;
        if to < 0 { to = 0; }
        if to > last { to = last; }
        let s = deck.slides.remove(idx);
        deck.slides.insert(to as usize, s);
        json!({"ok": true, "desc": format!("moved slide to position {}", to + 1)})
    }

    fn op_set_theme(&self, deck: &mut Deck, args: &Value) -> Value {
        let theme = sget(args, "theme_css");
        if theme.trim().is_empty() {
            return json!({"ok": false, "error": "empty theme_css"});
        }
        deck.theme_css = theme;
        json!({"ok": true, "desc": "restyled the deck"})
    }

    fn op_set_title(&self, deck: &mut Deck, args: &Value) -> Value {
        let title = sget(args, "title").trim().to_string();
        if title.is_empty() {
            return json!({"ok": false, "error": "empty title"});
        }
        deck.title = title.clone();
        json!({"ok": true, "desc": format!("renamed deck to \u{201c}{title}\u{201d}")})
    }

    // ——— LLM call ———
    async fn chat(&self, messages: &[Value], tools: &Value) -> Result<Value, String> {
        let url = provider::chat_url(&self.cfg)?;
        // No `temperature`: GPT-5-class reasoning models reject any value other
        // than the default (1) with a 400, and every other model is fine with
        // the default. Omitting it keeps us compatible across models/providers.
        let mut body = json!({
            "messages": messages,
            "tools": tools,
            "tool_choice": "auto",
        });
        if let Some(m) = provider::body_model(&self.cfg, &self.cfg.slide_model) {
            body["model"] = json!(m);
        }
        let mut req = self.http.post(&url);
        for (k, v) in provider::auth_headers(&self.cfg) {
            req = req.header(k, v);
        }
        let resp = req
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("chat request: {e}"))?;
        if !resp.status().is_success() {
            let code = resp.status().as_u16();
            let text = resp.text().await.unwrap_or_default();
            return Err(format!("chat http {code}: {}", truncate(&text, 300)));
        }
        let data: Value = resp.json().await.map_err(|e| format!("chat decode: {e}"))?;
        let msg = data
            .get("choices")
            .and_then(|c| c.as_array())
            .and_then(|a| a.first())
            .and_then(|c| c.get("message"))
            .cloned()
            .unwrap_or(json!({}));
        Ok(json!({
            "role": msg.get("role").and_then(|v| v.as_str()).unwrap_or("assistant"),
            "content": msg.get("content").cloned().unwrap_or(Value::Null),
            "tool_calls": msg.get("tool_calls").cloned().unwrap_or(json!([])),
        }))
    }
}

fn sget(v: &Value, key: &str) -> String {
    v.get(key).and_then(|x| x.as_str()).unwrap_or("").to_string()
}

/// Load + parse a deck.json into a Deck, or None if absent/unreadable/invalid.
fn load_deck_file(path: &PathBuf) -> Option<Deck> {
    let text = std::fs::read_to_string(path).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    Some(Deck::from_full_json(&v))
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n).collect()
    }
}

fn summarize(applied: &[String], deck: &Deck) -> String {
    let n = deck.slides.len();
    let count = format!("{n} slide{}", if n != 1 { "s" } else { "" });
    if applied.len() == 1 {
        let mut a = applied[0].clone();
        if let Some(c) = a.get_mut(0..1) {
            c.make_ascii_uppercase();
        }
        format!("{a} — {count} in \u{201c}{}\u{201d}.", deck.title)
    } else {
        format!("Applied {} changes — {count} in \u{201c}{}\u{201d}.", applied.len(), deck.title)
    }
}

pub const SYSTEM_PROMPT: &str = include_str!("slide_prompt.txt");

/// Tool schemas for the slide model. `read_reference` is only included when the
/// deck has enabled reference files, so the model isn't told about a tool that
/// would always return "no reference library".
fn tools(has_refs: bool) -> Value {
    let slide_props = json!({
        "title": {"type": "string", "description": "Plain-text slide title for the outline."},
        "html": {"type": "string", "description": "Inner HTML using the fixed class vocabulary. No inline styles or <style> tags."}
    });
    let mut list = json!([
        {"type": "function", "function": {
            "name": "write_deck",
            "description": "Replace the ENTIRE deck. Use only for a brand-new deck or a restructure that changes most slides. Include every slide that should exist after, in order.",
            "parameters": {"type": "object", "properties": {
                "title": {"type": "string"},
                "theme_css": {"type": "string"},
                "slides": {"type": "array", "items": {"type": "object", "properties": slide_props, "required": ["title", "html"]}}
            }, "required": ["title", "theme_css", "slides"]}
        }},
        {"type": "function", "function": {
            "name": "add_slide",
            "description": "Insert ONE new slide. By default appends to the end.",
            "parameters": {"type": "object", "properties": {
                "title": {"type": "string"}, "html": {"type": "string"},
                "after_id": {"type": "string", "description": "Insert after the slide with this id. Omit to append."}
            }, "required": ["title", "html"]}
        }},
        {"type": "function", "function": {
            "name": "edit_slide",
            "description": "Replace the title and/or html of ONE existing slide, identified by id. Other slides untouched.",
            "parameters": {"type": "object", "properties": {
                "id": {"type": "string"}, "title": {"type": "string"}, "html": {"type": "string"}
            }, "required": ["id"]}
        }},
        {"type": "function", "function": {
            "name": "delete_slide",
            "description": "Remove ONE slide by id.",
            "parameters": {"type": "object", "properties": {"id": {"type": "string"}}, "required": ["id"]}
        }},
        {"type": "function", "function": {
            "name": "move_slide",
            "description": "Reorder ONE slide to a new zero-based position.",
            "parameters": {"type": "object", "properties": {
                "id": {"type": "string"}, "to_index": {"type": "integer"}
            }, "required": ["id", "to_index"]}
        }},
        {"type": "function", "function": {
            "name": "set_theme",
            "description": "Replace theme_css for the whole deck. This is how you restyle / make a deck cohesive — one stylesheet restyles every slide.",
            "parameters": {"type": "object", "properties": {"theme_css": {"type": "string"}}, "required": ["theme_css"]}
        }},
        {"type": "function", "function": {
            "name": "set_title",
            "description": "Rename the deck.",
            "parameters": {"type": "object", "properties": {"title": {"type": "string"}}, "required": ["title"]}
        }},
        {"type": "function", "function": {
            "name": "generate_image",
            "description": "Generate a real image from a text prompt and return its image id. Place it with <img class=\"slide-image\" data-img=\"<id>\">. Multiple calls in one turn run concurrently. Reuse an existing image instead of regenerating.",
            "parameters": {"type": "object", "properties": {
                "prompt": {"type": "string"},
                "size": {"type": "string", "enum": ["1024x1024", "1536x1024", "1024x1536"]}
            }, "required": ["prompt"]}
        }}
    ]);
    if has_refs {
        if let Some(arr) = list.as_array_mut() {
            arr.push(json!({"type": "function", "function": {
                "name": "read_reference",
                "description": "Read the full text of one reference file from the [REFERENCE LIBRARY] manifest, by its id. Call this for any file whose name/preview looks relevant to the instruction before building, then use its facts. Read only what you need.",
                "parameters": {"type": "object", "properties": {
                    "id": {"type": "string", "description": "The reference file id from the manifest, e.g. \"r1\"."}
                }, "required": ["id"]}
            }}));
        }
    }
    list
}
