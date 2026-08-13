//! User-configured object-template catalog: YAML DTOs, format-version checks,
//! and availability resolution against registered component schemas.
//!
//! This crate parses catalog entries and tells the caller whether each one is
//! [`entry::LoadResult::Available`], [`entry::LoadResult::Unavailable`], or
//! [`entry::LoadResult::Invalid`] — it never touches the authoritative world.
//! Instantiating a template as a scene object is a later step, built on top
//! of these types through the ordinary `WorldCommand::CreateObject` path.

pub mod availability;
pub mod diagnostics;
pub mod document;
pub mod entry;
pub mod ids;
pub mod load;
pub mod source;
pub mod structure;

pub use availability::{AvailabilityOutcome, AvailabilityReason, resolve_availability};
pub use diagnostics::{Diagnostic, InvalidReason};
pub use document::{API_VERSION, KIND};
pub use entry::{CatalogEntry, CatalogFileError, CatalogLoadReport, LoadResult, TemplateMetadata};
pub use ids::{CatalogScopeName, NameError, TemplateName};
pub use load::{MAX_CATALOG_FILE_BYTES, load_catalog_directory, load_catalog_file};
pub use source::{DocumentOrdinal, SourceLocation, TemplateIdentity};
pub use structure::{TemplateComponentInstance, TemplatePropertyValue, TemplateShape, TemplateSpec};
