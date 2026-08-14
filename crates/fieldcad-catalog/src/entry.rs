//! A loaded catalog entry and the report produced by loading a directory.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::availability::AvailabilityReason;
use crate::diagnostics::{Diagnostic, InvalidReason};
use crate::document::CatalogMetadata;
use crate::source::{SourceLocation, TemplateIdentity};
use crate::structure::TemplateSpec;
use fieldcad_core::CatalogEntryRef;
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TemplateMetadata {
    pub description: Option<String>,
    pub author: Option<String>,
    pub labels: BTreeMap<String, String>,
    pub annotations: BTreeMap<String, String>,
}

impl TemplateMetadata {
    pub fn from_document(metadata: &CatalogMetadata) -> Self {
        Self {
            description: metadata.description.clone(),
            author: metadata.author.clone(),
            labels: metadata.labels.clone(),
            annotations: metadata.annotations.clone(),
        }
    }
}

/// One catalog entry as loaded from disk: where it came from, its best-known
/// identity, and its load result.
#[derive(Clone, Debug, PartialEq)]
pub struct CatalogEntry {
    pub source: SourceLocation,
    /// Source-qualified durable identity when the YAML supplied a valid
    /// catalog/template name and a structurally valid template.
    pub reference: Option<CatalogEntryRef>,
    /// `Some` as soon as `metadata.catalog`/`metadata.name` themselves
    /// validate, independent of whether the rest of the entry later turns
    /// out `Invalid` — a duplicate-detection or catalog-browsing layer
    /// needs this even for entries whose spec is broken, so it can show
    /// "personal-physics/fancy-unicorn: invalid" rather than an anonymous
    /// error.
    pub identity: Option<TemplateIdentity>,
    pub result: LoadResult,
}

/// SHA-256 over serde's deterministic encoding of BTreeMap-backed resolved
/// template data. This deliberately excludes source location so moving a file
/// produces an explicit relink candidate rather than a different template.
pub fn template_fingerprint(
    identity: &TemplateIdentity,
    metadata: &TemplateMetadata,
    spec: &TemplateSpec,
) -> String {
    let bytes = serde_json::to_vec(&(identity, metadata, spec))
        .expect("catalog template types are serializable");
    format!("{:x}", Sha256::digest(bytes))
}

#[derive(Clone, Debug, PartialEq)]
pub enum LoadResult {
    Available {
        metadata: TemplateMetadata,
        spec: TemplateSpec,
    },
    Unavailable {
        metadata: TemplateMetadata,
        spec: TemplateSpec,
        /// Never empty.
        reasons: Vec<AvailabilityReason>,
    },
    Invalid {
        /// Never empty.
        diagnostics: Vec<Diagnostic>,
    },
}

/// A whole-file failure that never got as far as identifying any document
/// inside it: unreadable, non-UTF-8, or over the size cap. No document
/// ordinal applies, since the file was never parsed far enough to count
/// documents.
#[derive(Clone, Debug, PartialEq)]
pub struct CatalogFileError {
    pub file: PathBuf,
    pub reason: InvalidReason,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CatalogLoadReport {
    pub entries: Vec<CatalogEntry>,
    pub file_errors: Vec<CatalogFileError>,
}

/// Relationship between a persisted catalog link and the currently loaded
/// catalog. A moved entry is deliberately not rebound automatically: callers
/// must offer an explicit relink action.
#[derive(Clone, Debug, PartialEq)]
pub enum LinkResolution<'a> {
    Exact(&'a CatalogEntry),
    RelinkCandidate(&'a CatalogEntry),
    Unavailable,
    Ambiguous,
}

impl CatalogLoadReport {
    /// Resolve a link by exact source first. If that source disappeared, one
    /// content-identical entry is an explicit relink candidate; more than one
    /// is ambiguous and none means unavailable.
    pub fn resolve_link(&self, link: &CatalogEntryRef) -> LinkResolution<'_> {
        if let Some(entry) = self
            .entries
            .iter()
            .find(|entry| entry.reference.as_ref() == Some(link))
        {
            return LinkResolution::Exact(entry);
        }
        let candidates: Vec<_> = self
            .entries
            .iter()
            .filter(|entry| {
                matches!(entry.result, LoadResult::Available { .. })
                    && entry.reference.as_ref().is_some_and(|reference| {
                        reference.fingerprint == link.fingerprint
                            && reference.catalog == link.catalog
                            && reference.template == link.template
                    })
            })
            .collect();
        match candidates.as_slice() {
            [] => LinkResolution::Unavailable,
            [entry] => LinkResolution::RelinkCandidate(entry),
            _ => LinkResolution::Ambiguous,
        }
    }
}
