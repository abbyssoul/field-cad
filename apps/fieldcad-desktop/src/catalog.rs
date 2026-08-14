//! User catalog discovery and first-run seeding.
//!
//! The catalog directory lives inside the application configuration directory
//! (same `ProjectDirs` convention as `crate::profile`). On first run the
//! bundled `starter_catalog.yaml` is written there so the five reference
//! particles appear without needing a separate download or editor.

use std::path::{Path, PathBuf};

use fieldcad_catalog::template_fingerprint;
use fieldcad_core::{CatalogEntryRef, CatalogOrigin};
use fieldcad_scene_document::DocumentCatalogEntry;

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

/// Stable reference for a scene-local entry.
pub fn document_entry_ref(entry: &DocumentCatalogEntry) -> CatalogEntryRef {
    CatalogEntryRef {
        catalog: entry.identity.catalog.as_str().to_owned(),
        template: entry.identity.template.as_str().to_owned(),
        origin: CatalogOrigin::Document {
            entry_id: entry.entry_id,
        },
        api_version: fieldcad_catalog::API_VERSION.to_owned(),
        fingerprint: template_fingerprint(&entry.identity, &entry.metadata, &entry.spec),
    }
}
