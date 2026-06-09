//! Reference material library — user-provided .txt/.md files the slide agent
//! can draw on when building a deck.
//!
//! Storage layout, under `<decks_dir>/refs/`:
//!   * `index.json`     — ordered list of {id, name, enabled, bytes}.
//!   * `<id>.txt`        — the raw file content (always stored as utf-8 text).
//!
//! Only ENABLED entries are offered to the model. Rather than dumping every
//! file into the prompt, the agent is given a lightweight MANIFEST (id, name,
//! size, a short preview) and a `read_reference` tool; it fetches the full text
//! of only the files it decides it needs. The index is the source of truth for
//! ordering + enabled state; the per-id files hold the (potentially large)
//! content so listing the library stays cheap.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Hard cap on a single uploaded file (1 MiB of text).
pub const MAX_FILE_BYTES: usize = 1_048_576;
/// How many leading chars of each file to show in the manifest preview, so the
/// model can judge relevance before deciding to read the whole thing.
const PREVIEW_CHARS: usize = 240;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RefMeta {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub bytes: usize,
}

/// The reference library rooted at `<decks_dir>/refs/`.
#[derive(Clone)]
pub struct RefStore {
    dir: PathBuf,
}

impl RefStore {
    pub fn new(decks_dir: &Path) -> Self {
        RefStore {
            dir: decks_dir.join("refs"),
        }
    }

    fn index_path(&self) -> PathBuf {
        self.dir.join("index.json")
    }

    fn content_path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.txt"))
    }

    /// Load the index (ordered metadata). Missing/invalid → empty library.
    pub fn list(&self) -> Vec<RefMeta> {
        match std::fs::read_to_string(self.index_path()) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
            Err(_) => Vec::new(),
        }
    }

    fn save_index(&self, items: &[RefMeta]) -> Result<(), String> {
        std::fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
        let json = serde_json::to_string_pretty(items).map_err(|e| e.to_string())?;
        std::fs::write(self.index_path(), json).map_err(|e| e.to_string())
    }

    /// Mint the next `r<N>` id not already present in the index.
    fn next_id(&self, items: &[RefMeta]) -> String {
        let mut best = 0u32;
        for m in items {
            if let Some(n) = m.id.strip_prefix('r').and_then(|s| s.parse::<u32>().ok()) {
                best = best.max(n);
            }
        }
        format!("r{}", best + 1)
    }

    /// Add a file. `name` is the display filename; `content` the utf-8 text.
    /// Returns the new entry's metadata.
    pub fn add(&self, name: &str, content: &str) -> Result<RefMeta, String> {
        let name = sanitize_name(name);
        let content = if content.len() > MAX_FILE_BYTES {
            // Keep a clean utf-8 boundary when truncating.
            let mut end = MAX_FILE_BYTES;
            while end > 0 && !content.is_char_boundary(end) {
                end -= 1;
            }
            &content[..end]
        } else {
            content
        };
        let mut items = self.list();
        let id = self.next_id(&items);
        std::fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
        std::fs::write(self.content_path(&id), content).map_err(|e| e.to_string())?;
        let meta = RefMeta {
            id: id.clone(),
            name,
            enabled: true,
            bytes: content.len(),
        };
        items.push(meta.clone());
        self.save_index(&items)?;
        Ok(meta)
    }

    /// Toggle (or otherwise set) the `enabled` flag for one entry.
    pub fn set_enabled(&self, id: &str, enabled: bool) -> Result<bool, String> {
        let mut items = self.list();
        let Some(m) = items.iter_mut().find(|m| m.id == id) else {
            return Ok(false);
        };
        m.enabled = enabled;
        self.save_index(&items)?;
        Ok(true)
    }

    /// Remove an entry and its content file.
    pub fn remove(&self, id: &str) -> Result<bool, String> {
        let mut items = self.list();
        let before = items.len();
        items.retain(|m| m.id != id);
        if items.len() == before {
            return Ok(false);
        }
        let _ = std::fs::remove_file(self.content_path(id)); // best-effort
        self.save_index(&items)?;
        Ok(true)
    }

    /// Full content of one enabled reference file. Returns None if the id is
    /// unknown, the file is disabled, or it can't be read — so the agent can
    /// never pull a file the user has toggled off.
    pub fn read(&self, id: &str) -> Option<String> {
        if !self.list().iter().any(|m| m.id == id && m.enabled) {
            return None;
        }
        std::fs::read_to_string(self.content_path(id)).ok()
    }

    /// A lightweight catalog of the enabled files for the model to scan: each
    /// line carries the id, name, size, and a short content preview so it can
    /// judge relevance and then call `read_reference` for the ones it wants.
    /// Returns None if nothing is enabled (so no block is added to the prompt).
    pub fn manifest_block(&self) -> Option<String> {
        let enabled: Vec<RefMeta> = self.list().into_iter().filter(|m| m.enabled).collect();
        if enabled.is_empty() {
            return None;
        }
        let mut out = String::from(
            "[REFERENCE LIBRARY — files the user provided as background for this deck. \
Call read_reference(id) to read the full text of any that look relevant to the instruction, \
then use those facts when building or editing slides (do not invent facts that contradict them). \
Only read what you need.]\n",
        );
        for m in &enabled {
            let preview = self
                .content_path(&m.id)
                .to_str()
                .and_then(|_| std::fs::read_to_string(self.content_path(&m.id)).ok())
                .map(|c| preview_of(&c))
                .unwrap_or_default();
            out.push_str(&format!(
                "- id \"{}\" — {} ({}): {}\n",
                m.id,
                m.name,
                human_bytes(m.bytes),
                preview
            ));
        }
        Some(out)
    }
}

/// First line/snippet of a file, whitespace-collapsed and clipped, for the
/// manifest preview.
fn preview_of(content: &str) -> String {
    let collapsed: String = content.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() > PREVIEW_CHARS {
        let clipped: String = collapsed.chars().take(PREVIEW_CHARS).collect();
        format!("{clipped}…")
    } else {
        collapsed
    }
}

fn human_bytes(n: usize) -> String {
    if n < 1024 {
        format!("{n} B")
    } else if n < 1024 * 1024 {
        format!("{:.1} KB", n as f64 / 1024.0)
    } else {
        format!("{:.1} MB", n as f64 / 1_048_576.0)
    }
}

/// Keep a filename safe to show and free of path separators.
fn sanitize_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name).trim();
    if base.is_empty() {
        "untitled.txt".to_string()
    } else {
        base.chars().take(120).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (RefStore, PathBuf) {
        let tmp = std::env::temp_dir().join(format!(
            "sc-refs-test-{}-{}",
            std::process::id(),
            // vary per-call so parallel tests don't collide
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        (RefStore::new(&tmp), tmp)
    }

    #[test]
    fn add_list_toggle_remove() {
        let (s, tmp) = store();
        let m = s.add("notes.md", "# Hello\nworld").unwrap();
        assert_eq!(m.id, "r1");
        assert!(m.enabled);
        assert_eq!(s.list().len(), 1);

        // manifest lists the file (id + name + preview); read() returns content
        let manifest = s.manifest_block().unwrap();
        assert!(manifest.contains("id \"r1\""));
        assert!(manifest.contains("notes.md"));
        assert!(manifest.contains("Hello")); // preview snippet
        assert_eq!(s.read("r1").as_deref(), Some("# Hello\nworld"));

        // disable → excluded from manifest AND unreadable
        assert!(s.set_enabled("r1", false).unwrap());
        assert!(s.manifest_block().is_none());
        assert!(s.read("r1").is_none());

        // remove
        assert!(s.remove("r1").unwrap());
        assert_eq!(s.list().len(), 0);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn manifest_lists_all_enabled_with_preview_and_read_is_gated() {
        let (s, tmp) = store();
        s.add("a.md", "alpha content about widgets").unwrap();
        let b = s.add("b.txt", "beta notes").unwrap();
        s.set_enabled(&b.id, false).unwrap();

        let manifest = s.manifest_block().unwrap();
        // enabled file present with id + preview; disabled file absent
        assert!(manifest.contains("id \"r1\""));
        assert!(manifest.contains("a.md"));
        assert!(manifest.contains("alpha content about widgets"));
        assert!(!manifest.contains("b.txt"));

        // read honors the enabled gate
        assert!(s.read("r1").is_some());
        assert!(s.read("r2").is_none()); // disabled
        assert!(s.read("nope").is_none()); // unknown
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn ids_increment_and_names_sanitized() {
        let (s, tmp) = store();
        s.add("../../etc/passwd", "x").unwrap();
        let m2 = s.add("b.txt", "y").unwrap();
        assert_eq!(m2.id, "r2");
        assert_eq!(s.list()[0].name, "passwd");
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
