//! User-supplied image library — pictures the user wants available on slides,
//! each carrying a text DESCRIPTION the slide agent reads so it knows what the
//! image shows (and a vision model can caption it on upload). This is distinct
//! from `Deck.images`, which holds AI-generated images for one deck; the library
//! is a persistent pool the user curates and the agent draws on at build time.
//!
//! Storage layout, under `<decks_dir>/images/`:
//!   * `index.json`     — ordered list of metadata (no base64, so listing stays
//!                        cheap): {id, name, description, enabled, pinned_slide, bytes}.
//!   * `<id>.dat`        — the raw data URI ("data:image/png;base64,…").
//!
//! Only ENABLED entries are injected into a build. `pinned_slide` (1-based slide
//! number, or 0 = unpinned) lets the user nudge the agent to place a given image
//! on a specific slide; unpinned images are placed wherever they fit.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Hard cap on a single image's data URI (~8 MiB; base64 of a ~6 MiB image).
pub const MAX_IMAGE_BYTES: usize = 8 * 1_048_576;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ImageMeta {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub enabled: bool,
    /// 1-based slide number to pin to, or 0 for "let the agent decide".
    #[serde(default)]
    pub pinned_slide: u32,
    pub bytes: usize,
}

/// One enabled library image ready to inject into a build.
pub struct EnabledImage {
    pub meta: ImageMeta,
    pub data_uri: String,
}

/// The image library rooted at `<decks_dir>/images/`.
#[derive(Clone)]
pub struct ImageStore {
    dir: PathBuf,
}

impl ImageStore {
    pub fn new(decks_dir: &Path) -> Self {
        ImageStore {
            dir: decks_dir.join("images"),
        }
    }

    fn index_path(&self) -> PathBuf {
        self.dir.join("index.json")
    }

    fn data_path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.dat"))
    }

    pub fn list(&self) -> Vec<ImageMeta> {
        match std::fs::read_to_string(self.index_path()) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
            Err(_) => Vec::new(),
        }
    }

    fn save_index(&self, items: &[ImageMeta]) -> Result<(), String> {
        std::fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
        let json = serde_json::to_string_pretty(items).map_err(|e| e.to_string())?;
        std::fs::write(self.index_path(), json).map_err(|e| e.to_string())
    }

    /// Mint the next `lib<N>` id not present in the index. Using a distinct
    /// prefix from deck image ids (`img<N>`) avoids any collision when these are
    /// injected into the deck pool.
    fn next_id(&self, items: &[ImageMeta]) -> String {
        let mut best = 0u32;
        for m in items {
            if let Some(n) = m.id.strip_prefix("lib").and_then(|s| s.parse::<u32>().ok()) {
                best = best.max(n);
            }
        }
        format!("lib{}", best + 1)
    }

    /// Add an image. `data_uri` must be a `data:` URI. `description` may be
    /// empty (e.g. filled in later by a vision caption). Returns the metadata.
    pub fn add(&self, name: &str, data_uri: &str, description: &str) -> Result<ImageMeta, String> {
        if !data_uri.starts_with("data:") {
            return Err("expected a data: URI".to_string());
        }
        if data_uri.len() > MAX_IMAGE_BYTES {
            return Err("image too large".to_string());
        }
        let mut items = self.list();
        let id = self.next_id(&items);
        std::fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
        std::fs::write(self.data_path(&id), data_uri).map_err(|e| e.to_string())?;
        let meta = ImageMeta {
            id: id.clone(),
            name: sanitize_name(name),
            description: description.trim().to_string(),
            enabled: true,
            pinned_slide: 0,
            bytes: data_uri.len(),
        };
        items.push(meta.clone());
        self.save_index(&items)?;
        Ok(meta)
    }

    /// Update mutable metadata fields. Any `Some` is applied; `None` is left
    /// unchanged. Returns false if no entry has the id.
    pub fn update(
        &self,
        id: &str,
        enabled: Option<bool>,
        description: Option<String>,
        pinned_slide: Option<u32>,
    ) -> Result<bool, String> {
        let mut items = self.list();
        let Some(m) = items.iter_mut().find(|m| m.id == id) else {
            return Ok(false);
        };
        if let Some(v) = enabled {
            m.enabled = v;
        }
        if let Some(v) = description {
            m.description = v.trim().to_string();
        }
        if let Some(v) = pinned_slide {
            m.pinned_slide = v;
        }
        self.save_index(&items)?;
        Ok(true)
    }

    pub fn remove(&self, id: &str) -> Result<bool, String> {
        let mut items = self.list();
        let before = items.len();
        items.retain(|m| m.id != id);
        if items.len() == before {
            return Ok(false);
        }
        let _ = std::fs::remove_file(self.data_path(id)); // best-effort
        self.save_index(&items)?;
        Ok(true)
    }

    pub fn read_data_uri(&self, id: &str) -> Option<String> {
        std::fs::read_to_string(self.data_path(id)).ok()
    }

    /// Enabled images (metadata + data URI), in index order. These get injected
    /// into the deck's image pool before a build.
    pub fn enabled_images(&self) -> Vec<EnabledImage> {
        self.list()
            .into_iter()
            .filter(|m| m.enabled)
            .filter_map(|m| {
                let data_uri = self.read_data_uri(&m.id)?;
                Some(EnabledImage { meta: m, data_uri })
            })
            .collect()
    }
}

/// Keep a filename safe to show and free of path separators.
fn sanitize_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name).trim();
    if base.is_empty() {
        "image".to_string()
    } else {
        base.chars().take(120).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (ImageStore, PathBuf) {
        let tmp = std::env::temp_dir().join(format!(
            "sc-img-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        (ImageStore::new(&tmp), tmp)
    }

    #[test]
    fn add_update_remove() {
        let (s, tmp) = store();
        let m = s.add("logo.png", "data:image/png;base64,AAAA", "company logo").unwrap();
        assert_eq!(m.id, "lib1");
        assert!(m.enabled);
        assert_eq!(m.pinned_slide, 0);
        assert_eq!(m.description, "company logo");

        // enabled_images returns it with data.
        let en = s.enabled_images();
        assert_eq!(en.len(), 1);
        assert_eq!(en[0].data_uri, "data:image/png;base64,AAAA");

        // update description + pin + disable.
        assert!(s.update("lib1", Some(false), Some("new desc".into()), Some(3)).unwrap());
        let m2 = &s.list()[0];
        assert_eq!(m2.description, "new desc");
        assert_eq!(m2.pinned_slide, 3);
        assert!(!m2.enabled);
        assert!(s.enabled_images().is_empty()); // disabled → excluded

        assert!(s.remove("lib1").unwrap());
        assert_eq!(s.list().len(), 0);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn rejects_non_data_uri_and_sanitizes_name() {
        let (s, tmp) = store();
        assert!(s.add("x.png", "http://evil/x.png", "").is_err());
        let m = s.add("../../secret.png", "data:image/png;base64,AAAA", "").unwrap();
        assert_eq!(m.name, "secret.png");
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
