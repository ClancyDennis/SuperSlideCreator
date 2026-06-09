//! Projects: isolated slide-deck workspaces.
//!
//! Each project is a directory `<decks_dir>/projects/<id>/` holding its own
//! `deck.json`, `deck.html`, `refs/`, and `images/`. A registry file
//! `<decks_dir>/projects/index.json` records the ordered project list and which
//! one is active. Everything per-deck resolves through the *active* project
//! dir, so two decks never share state.
//!
//! A legacy global deck (the pre-projects `<decks_dir>/deck.json` + refs/images)
//! is migrated into a "Default" project on first load.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    /// Monotonic counter used only to order the list (most-recent first); we
    /// can't call the clock here, so callers stamp it.
    #[serde(default)]
    pub seq: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Index {
    #[serde(default)]
    projects: Vec<Project>,
    #[serde(default)]
    active: String,
    #[serde(default)]
    seq: u64,
}

/// Manages the project registry rooted at `<decks_dir>/projects/`.
#[derive(Clone)]
pub struct ProjectStore {
    root: PathBuf, // <decks_dir>/projects
    decks_dir: PathBuf,
}

impl ProjectStore {
    pub fn new(decks_dir: &Path) -> Self {
        ProjectStore {
            root: decks_dir.join("projects"),
            decks_dir: decks_dir.to_path_buf(),
        }
    }

    fn index_path(&self) -> PathBuf {
        self.root.join("index.json")
    }

    fn read_index(&self) -> Index {
        match fs::read_to_string(self.index_path()) {
            Ok(t) => serde_json::from_str(&t).unwrap_or_default(),
            Err(_) => Index::default(),
        }
    }

    fn write_index(&self, idx: &Index) -> Result<(), String> {
        fs::create_dir_all(&self.root).map_err(|e| e.to_string())?;
        let t = serde_json::to_string_pretty(idx).map_err(|e| e.to_string())?;
        fs::write(self.index_path(), t).map_err(|e| e.to_string())
    }

    /// Directory for a given project id.
    pub fn project_dir(&self, id: &str) -> PathBuf {
        self.root.join(id)
    }

    /// Ensure at least one project exists and an active one is set. Migrates a
    /// legacy global deck if present. Returns the active project's id.
    pub fn ensure_initialized(&self) -> Result<String, String> {
        let mut idx = self.read_index();

        // Already have projects: just make sure `active` points at a real one.
        if !idx.projects.is_empty() {
            if !idx.projects.iter().any(|p| p.id == idx.active) {
                idx.active = idx.projects[0].id.clone();
                self.write_index(&idx)?;
            }
            return Ok(idx.active);
        }

        // No projects yet. Create the first one and migrate any legacy deck.
        idx.seq += 1;
        let first = Project { id: "default".to_string(), name: "Default".to_string(), seq: idx.seq };
        let dir = self.project_dir(&first.id);
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        self.migrate_legacy_into(&dir);
        idx.active = first.id.clone();
        idx.projects.push(first);
        self.write_index(&idx)?;
        Ok(idx.active)
    }

    /// Move a pre-projects global deck (`<decks_dir>/{deck.json,deck.html}` +
    /// `refs/` + `images/`) into a project dir. Best-effort; ignores absent
    /// pieces. Renames so the old paths stop being used.
    fn migrate_legacy_into(&self, dest: &Path) {
        for name in ["deck.json", "deck.html"] {
            let from = self.decks_dir.join(name);
            if from.is_file() {
                let _ = fs::rename(&from, dest.join(name));
            }
        }
        for sub in ["refs", "images"] {
            let from = self.decks_dir.join(sub);
            if from.is_dir() {
                let _ = fs::rename(&from, dest.join(sub));
            }
        }
    }

    pub fn active_id(&self) -> Result<String, String> {
        self.ensure_initialized()
    }

    /// Absolute dir of the active project (created if needed).
    pub fn active_dir(&self) -> Result<PathBuf, String> {
        let id = self.active_id()?;
        let dir = self.project_dir(&id);
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        Ok(dir)
    }

    /// Ordered list (most-recently-touched first) + the active id, as JSON for
    /// the frontend.
    pub fn list_json(&self) -> Value {
        let _ = self.ensure_initialized();
        let idx = self.read_index();
        let mut projects = idx.projects.clone();
        projects.sort_by(|a, b| b.seq.cmp(&a.seq));
        json!({
            "active": idx.active,
            "projects": projects.iter().map(|p| json!({"id": p.id, "name": p.name})).collect::<Vec<_>>(),
        })
    }

    /// Create a new (empty) project, make it active, and return its id.
    pub fn create(&self, name: &str) -> Result<String, String> {
        self.ensure_initialized()?;
        let mut idx = self.read_index();
        let name = name.trim();
        let name = if name.is_empty() { "Untitled Project" } else { name };
        idx.seq += 1;
        let id = self.unique_id(&idx, name);
        fs::create_dir_all(self.project_dir(&id)).map_err(|e| e.to_string())?;
        idx.projects.push(Project { id: id.clone(), name: name.to_string(), seq: idx.seq });
        idx.active = id.clone();
        self.write_index(&idx)?;
        Ok(id)
    }

    /// Switch the active project. Errors if the id is unknown.
    pub fn set_active(&self, id: &str) -> Result<(), String> {
        let mut idx = self.read_index();
        let Some(p) = idx.projects.iter_mut().find(|p| p.id == id) else {
            return Err("unknown project".to_string());
        };
        // Bump so it sorts to the top of the recents list.
        idx.seq += 1;
        p.seq = idx.seq;
        idx.active = id.to_string();
        self.write_index(&idx)
    }

    pub fn rename(&self, id: &str, name: &str) -> Result<(), String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("name required".to_string());
        }
        let mut idx = self.read_index();
        let Some(p) = idx.projects.iter_mut().find(|p| p.id == id) else {
            return Err("unknown project".to_string());
        };
        p.name = name.to_string();
        self.write_index(&idx)
    }

    /// Delete a project and its files. Won't delete the last remaining project.
    /// If the deleted one was active, activates the most-recent survivor.
    pub fn delete(&self, id: &str) -> Result<(), String> {
        let mut idx = self.read_index();
        if idx.projects.len() <= 1 {
            return Err("can't delete the last project".to_string());
        }
        if !idx.projects.iter().any(|p| p.id == id) {
            return Err("unknown project".to_string());
        }
        idx.projects.retain(|p| p.id != id);
        let _ = fs::remove_dir_all(self.project_dir(id));
        if idx.active == id {
            // Pick the highest-seq survivor.
            idx.active = idx
                .projects
                .iter()
                .max_by_key(|p| p.seq)
                .map(|p| p.id.clone())
                .unwrap_or_default();
        }
        self.write_index(&idx)
    }

    /// A filesystem-safe, unique id derived from the name (slug + counter).
    fn unique_id(&self, idx: &Index, name: &str) -> String {
        let base: String = name
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
            .collect();
        let base = base.trim_matches('-').to_string();
        let base = if base.is_empty() { "project".to_string() } else { base };
        let mut id = base.clone();
        let mut n = 2;
        while idx.projects.iter().any(|p| p.id == id) || self.project_dir(&id).exists() {
            id = format!("{base}-{n}");
            n += 1;
        }
        id
    }
}
