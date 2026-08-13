//! A loaded catalog entry and the report produced by loading a directory.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::availability::AvailabilityReason;
use crate::diagnostics::{Diagnostic, InvalidReason};
use crate::document::CatalogMetadata;
use crate::source::{SourceLocation, TemplateIdentity};
use crate::structure::TemplateSpec;

#[derive(Clone, Debug, PartialEq)]
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
    /// `Some` as soon as `metadata.catalog`/`metadata.name` themselves
    /// validate, independent of whether the rest of the entry later turns
    /// out `Invalid` — a duplicate-detection or catalog-browsing layer
    /// needs this even for entries whose spec is broken, so it can show
    /// "personal-physics/fancy-unicorn: invalid" rather than an anonymous
    /// error.
    pub identity: Option<TemplateIdentity>,
    pub result: LoadResult,
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
