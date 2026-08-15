//! User catalog discovery and first-run seeding.
//!
//! The catalog directory lives inside the application configuration directory
//! (same `ProjectDirs` convention as `crate::profile`). On first run the
//! bundled `starter_catalog.yaml` is written there so the five reference
//! particles appear without needing a separate download or editor.

use std::path::{Path, PathBuf};

pub fn catalog_directory() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "fieldcad").map(|dirs| dirs.config_dir().join("catalog"))
}

/// Write the bundled starter catalog into `dir` only when the directory
/// does not already exist — it never overwrites edits or reappears after
/// deletion.  Uses a plain `create_dir_all` + `write` (seed data, not a
/// user's live document) rather than the atomic ceremony a saved scene
/// uses.
pub fn seed_starter_catalog_if_missing(dir: &Path) {
    if dir.exists() {
        return;
    }
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let dest = dir.join("starter.yaml");
    let _ = std::fs::write(&dest, include_str!("starter_catalog.yaml"));
}
