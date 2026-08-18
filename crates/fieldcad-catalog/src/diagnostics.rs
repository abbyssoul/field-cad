//! Diagnostics for a catalog entry that failed to parse or validate.

use fieldcad_core::IdentifierError;

use crate::ids::NameError;

/// A single problem found in a catalog document, optionally pinned to a
/// field within it.
#[derive(Clone, Debug, PartialEq)]
pub struct Diagnostic {
    /// Dotted/bracket path into the document, e.g.
    /// `spec.components[0].properties.mass`. `None` for issues that precede
    /// any field context (file too large, not UTF-8, YAML that fails to
    /// parse as a document at all).
    pub field_path: Option<String>,
    pub reason: InvalidReason,
}

impl std::fmt::Display for Diagnostic {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.field_path {
            Some(path) => write!(formatter, "{path}: {}", self.reason),
            None => write!(formatter, "{}", self.reason),
        }
    }
}

/// Why a catalog entry (or the file containing it) could not be treated as
/// parsable/structurally valid.
///
/// This is deliberately independent of whether any component/kind is
/// currently *registered* — see [`crate::availability::AvailabilityReason`]
/// for that registry-dependent tier.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum InvalidReason {
    #[error("file exceeds the {max_bytes}-byte catalog size limit ({actual_bytes} bytes)")]
    FileTooLarge { max_bytes: u64, actual_bytes: u64 },
    #[error("file is not valid UTF-8")]
    NotUtf8,
    #[error("could not read catalog file: {message}")]
    Io { message: String },
    #[error("malformed YAML: {message}")]
    MalformedYaml { message: String },
    #[error("unsupported apiVersion '{found}', expected '{expected}'")]
    UnsupportedApiVersion { found: String, expected: String },
    #[error("unsupported kind '{found}', expected '{expected}'")]
    UnsupportedKind { found: String, expected: String },
    #[error("does not match the catalog entry format: {message}")]
    SchemaMismatch { message: String },
    #[error("catalog name is not valid: {source}")]
    InvalidCatalogName {
        #[source]
        source: NameError,
    },
    #[error("template name is not valid: {source}")]
    InvalidTemplateName {
        #[source]
        source: NameError,
    },
    #[error("plugin id is not valid: {source}")]
    InvalidPluginId {
        #[source]
        source: IdentifierError,
    },
    #[error("component name is not valid: {source}")]
    InvalidComponentName {
        #[source]
        source: IdentifierError,
    },
    #[error("property id is not valid: {source}")]
    InvalidPropertyId {
        #[source]
        source: IdentifierError,
    },
    #[error("shape value must be positive and finite, got {value}")]
    NonPositiveOrNonFiniteExtent { value: f64 },
    #[error("color channel must be finite and within 0.0..=1.0, got {value}")]
    ColorChannelOutOfRange { value: f64 },
    #[error("value must be finite, got {value}")]
    NonFiniteValue { value: f64 },
    #[error("component '{component}' is listed more than once in this entry")]
    DuplicateComponentInEntry { component: String },
    #[error("choice value cannot be empty")]
    EmptyChoiceValue,
}
