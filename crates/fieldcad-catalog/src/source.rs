//! Where a catalog entry came from.

use std::num::NonZeroUsize;
use std::path::PathBuf;

use crate::ids::{CatalogScopeName, TemplateName};
use fieldcad_core::{CatalogEntryRef, CatalogOrigin};

/// A document's 1-based position within a `---`-separated YAML stream.
///
/// 1-based because this is shown to a user ("document #2 of file.yaml"), not
/// used as a raw index.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct DocumentOrdinal(NonZeroUsize);

impl DocumentOrdinal {
    /// `index` is the document's 0-based position within the stream.
    pub fn new(index: usize) -> Self {
        Self(NonZeroUsize::new(index + 1).expect("index + 1 is never zero"))
    }

    pub fn get(self) -> usize {
        self.0.get()
    }
}

impl std::fmt::Display for DocumentOrdinal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

/// The file and document-within-stream a catalog entry was parsed from.
///
/// Together with [`TemplateIdentity`], this is what a live link within an
/// installed catalog is identified by.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceLocation {
    pub file: PathBuf,
    pub document_ordinal: DocumentOrdinal,
}

/// The user-authored catalog and template name pair that names a template.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub struct TemplateIdentity {
    pub catalog: CatalogScopeName,
    pub template: TemplateName,
}

/// Build a portable reference for an entry loaded below `catalog_root`.
/// Entries outside that root are not expected from the directory loader; use
/// the file name as a conservative fallback rather than serializing an
/// absolute machine-local path.
pub fn global_entry_ref(
    catalog_root: &std::path::Path,
    source: &SourceLocation,
    identity: &TemplateIdentity,
    fingerprint: String,
) -> CatalogEntryRef {
    let relative_path = source
        .file
        .strip_prefix(catalog_root)
        .unwrap_or(source.file.as_path())
        .to_string_lossy()
        .into_owned();
    CatalogEntryRef {
        catalog: identity.catalog.as_str().to_owned(),
        template: identity.template.as_str().to_owned(),
        origin: CatalogOrigin::Global {
            relative_path,
            document_ordinal: source.document_ordinal.get(),
        },
        api_version: crate::document::API_VERSION.to_owned(),
        fingerprint,
    }
}

impl std::fmt::Display for TemplateIdentity {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}/{}", self.catalog, self.template)
    }
}
