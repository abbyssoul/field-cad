//! Durable, generic provenance for an object created from a catalog
//! template — see `docs/tasks/user-configurable-object-catalog.md`,
//! "Linked instances and portable scenes" → "Instantiation".
//!
//! Plain data, not `fieldcad_catalog::TemplateIdentity`: `fieldcad-catalog`
//! depends on this crate, not the other way around, so what a catalog entry
//! an object was instantiated from *was* has to be representable here in
//! terms this crate already owns (owned `String`s), not that crate's
//! newtypes.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A stable catalog-entry location. User-authored catalog/template names are
/// labels and may collide, so scene-local preferences and links use this
/// source-qualified identity instead.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CatalogOrigin {
    /// A YAML document under Field CAD's configured catalog root.
    Global {
        /// Path relative to the configured catalog root.
        relative_path: String,
        /// One-based YAML document position within that file.
        document_ordinal: usize,
    },
    /// An entry persisted in one scene document.
    Document { entry_id: Uuid },
}

/// Durable, source-qualified identity and resolved-content fingerprint for a
/// catalog entry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogEntryRef {
    pub catalog: String,
    pub template: String,
    pub origin: CatalogOrigin,
    pub api_version: String,
    /// SHA-256 of the canonical resolved template content.
    pub fingerprint: String,
}

impl CatalogEntryRef {
    /// Whether two references identify the same editable source entry,
    /// independently of a changed resolved-content fingerprint.
    pub fn same_source(&self, other: &Self) -> bool {
        self.catalog == other.catalog
            && self.template == other.template
            && self.origin == other.origin
            && self.api_version == other.api_version
    }
}

/// Whether a catalog provenance record still governs template-owned values.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum CatalogLinkMode {
    /// Shape and component values track the catalog until explicitly unlinked.
    #[default]
    Tracking,
    /// The record is historical provenance only; instance values are editable.
    Unlinked,
}

/// Where an object's authored values originally came from: a catalog
/// template, instantiated once. Resolved values are copied into the object
/// at creation time — this is provenance for display/propagation (later
/// steps), never a live reference a solver reads, and a scene carrying it
/// must open identically whether or not the originating catalog is still
/// installed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CatalogLink {
    /// Source-qualified reference, format version, and original content
    /// fingerprint. Optional only because a hand-authored or partially
    /// completed object need not track a catalog entry; released catalog
    /// scenes always store the source-qualified reference.
    #[serde(default)]
    pub entry: Option<CatalogEntryRef>,
    /// Tracking keeps template-owned values read-only. Unlinking retains the
    /// entry below as provenance while allowing ordinary instance edits.
    #[serde(default)]
    pub mode: CatalogLinkMode,
    /// Human-readable description of the entry's on-disk source — file path
    /// and in-stream document position today, e.g.
    /// `"starter-particles.yaml (document 1)"`. Enough to explain a missing
    /// or changed source to a user without a live filesystem reference. A
    /// dedicated change-detection fingerprint/template-revision field
    /// belongs to the reload/conflict-detection step and can be added here
    /// later behind `#[serde(default)]` without a breaking change.
    pub source_description: String,
}
