//! User-configured object-template catalog: YAML DTOs, format-version checks,
//! and availability resolution against registered component schemas.
//!
//! This crate parses catalog entries, tells the caller whether each one is
//! [`entry::LoadResult::Available`], [`entry::LoadResult::Unavailable`], or
//! [`entry::LoadResult::Invalid`], and can instantiate an available entry as
//! a `fieldcad_core::ObjectSpec` ready for the ordinary authoritative
//! `WorldCommand::CreateObject` path — it never mutates the world itself.

pub mod availability;
pub mod diagnostics;
pub mod document;
pub mod entry;
pub mod ids;
pub mod instantiate;
pub mod load;
pub mod naming;
pub mod source;
pub mod structure;
pub mod write;

pub use availability::{
    AvailabilityOutcome, AvailabilityReason, property_bag_to_template, resolve_availability,
    template_properties_to_bag,
};
pub use diagnostics::{Diagnostic, InvalidReason};
pub use document::{API_VERSION, KIND};
pub use entry::{
    CatalogEntry, CatalogFileError, CatalogLoadReport, LinkResolution, LoadResult,
    TemplateMetadata, template_fingerprint,
};
pub use ids::{CatalogScopeName, NameError, TemplateName};
pub use instantiate::{InstantiationPlacement, instantiate_template};
pub use load::{MAX_CATALOG_FILE_BYTES, load_catalog_directory, load_catalog_file};
pub use naming::suggest_display_name;
pub use source::{DocumentOrdinal, SourceLocation, TemplateIdentity, global_entry_ref};
pub use structure::{
    TemplateComponentInstance, TemplatePropertyValue, TemplateShape, TemplateSpec,
};
pub use write::{
    DirectoryState, FingerprintState, SourceTarget, WriteError, create_entry, directory_state,
    file_state, remove_entry_at, save_entry_at,
};
