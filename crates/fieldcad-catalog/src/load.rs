//! Loads catalog entries from a flat directory of YAML files.
//!
//! Every failure is isolated to the smallest scope it can be: a malformed
//! document never hides its siblings in the same file, and an oversized or
//! unreadable file never blocks the rest of the directory. The app must
//! never fail to start over catalog state.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use fieldcad_core::{ComponentSchema, ComponentTypeId};

use crate::availability::{AvailabilityOutcome, resolve_availability};
use crate::diagnostics::{Diagnostic, InvalidReason};
use crate::document::{API_VERSION, CatalogEntryDocument, CatalogEnvelope, CatalogMetadata, KIND};
use crate::entry::{
    CatalogEntry, CatalogFileError, CatalogLoadReport, LoadResult, TemplateMetadata,
};
use crate::ids::{CatalogScopeName, TemplateName};
use crate::source::{DocumentOrdinal, SourceLocation, TemplateIdentity, global_entry_ref};
use crate::structure::validate_structure;

/// Catalog files above this size are refused before parsing.
pub const MAX_CATALOG_FILE_BYTES: u64 = 1024 * 1024;

/// Load every `.yaml`/`.yml` file in `dir` (flat scan, no recursion),
/// resolving availability against `component_schemas`.
///
/// A missing or unreadable directory yields an empty report rather than an
/// error.
pub fn load_catalog_directory(
    dir: &Path,
    component_schemas: &BTreeMap<ComponentTypeId, ComponentSchema>,
) -> CatalogLoadReport {
    let mut report = CatalogLoadReport::default();
    let Ok(read_dir) = fs::read_dir(dir) else {
        return report;
    };

    let mut paths: Vec<PathBuf> = read_dir
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| {
                    ext.eq_ignore_ascii_case("yaml") || ext.eq_ignore_ascii_case("yml")
                })
        })
        .collect();
    // Deterministic order so a report is stable across runs over the same
    // directory contents.
    paths.sort();

    for path in paths {
        load_catalog_file_from_root(dir, &path, component_schemas, &mut report);
    }

    report
}

/// Load one file's document stream into `report`. A per-document failure
/// never removes its siblings; a whole-file failure never blocks the rest
/// of the directory (the caller continues past it).
pub fn load_catalog_file(
    path: &Path,
    component_schemas: &BTreeMap<ComponentTypeId, ComponentSchema>,
    report: &mut CatalogLoadReport,
) {
    let root = path.parent().unwrap_or_else(|| Path::new(""));
    load_catalog_file_from_root(root, path, component_schemas, report);
}

fn load_catalog_file_from_root(
    catalog_root: &Path,
    path: &Path,
    component_schemas: &BTreeMap<ComponentTypeId, ComponentSchema>,
    report: &mut CatalogLoadReport,
) {
    let bytes = match read_capped(path) {
        Ok(bytes) => bytes,
        Err(reason) => {
            report.file_errors.push(CatalogFileError {
                file: path.to_owned(),
                reason,
            });
            return;
        }
    };

    let text = match std::str::from_utf8(&bytes) {
        Ok(text) => text,
        Err(_) => {
            report.file_errors.push(CatalogFileError {
                file: path.to_owned(),
                reason: InvalidReason::NotUtf8,
            });
            return;
        }
    };

    for (index, document) in serde_norway::Deserializer::from_str(text).enumerate() {
        let ordinal = DocumentOrdinal::new(index);
        let (entry, recoverable) =
            load_one_document(catalog_root, path, ordinal, document, component_schemas);
        report.entries.push(entry);
        if !recoverable {
            // A lexical/syntax error (as opposed to a semantic mismatch)
            // leaves the shared YAML parser in an indeterminate state:
            // confirmed empirically against the vendored `serde_norway`
            // parser that it does not reliably resynchronize to the next
            // `---` boundary afterward, and can otherwise loop indefinitely
            // re-reporting the same error. Every document recorded above
            // stays isolated and valid; anything after this point in the
            // same file is not recoverable and is left unreported rather
            // than risking an unbounded loop.
            break;
        }
    }
}

fn read_capped(path: &Path) -> Result<Vec<u8>, InvalidReason> {
    let metadata = fs::metadata(path).map_err(|error| InvalidReason::Io {
        message: error.to_string(),
    })?;
    if metadata.len() > MAX_CATALOG_FILE_BYTES {
        return Err(InvalidReason::FileTooLarge {
            max_bytes: MAX_CATALOG_FILE_BYTES,
            actual_bytes: metadata.len(),
        });
    }

    let bytes = fs::read(path).map_err(|error| InvalidReason::Io {
        message: error.to_string(),
    })?;
    // Defends a TOCTOU race where the file grew between the stat above and
    // this read.
    if bytes.len() as u64 > MAX_CATALOG_FILE_BYTES {
        return Err(InvalidReason::FileTooLarge {
            max_bytes: MAX_CATALOG_FILE_BYTES,
            actual_bytes: bytes.len() as u64,
        });
    }
    Ok(bytes)
}

/// Returns the loaded entry together with whether it is safe for the caller
/// to keep reading further documents from the same file's stream (`false`
/// only for a lexical/syntax-level YAML error — see the call site).
fn load_one_document(
    catalog_root: &Path,
    file: &Path,
    ordinal: DocumentOrdinal,
    document: serde_norway::Deserializer<'_>,
    component_schemas: &BTreeMap<ComponentTypeId, ComponentSchema>,
) -> (CatalogEntry, bool) {
    let source = SourceLocation {
        file: file.to_owned(),
        document_ordinal: ordinal,
    };

    let value: serde_norway::Value = match serde::Deserialize::deserialize(document) {
        Ok(value) => value,
        Err(error) => {
            // A failure at this first step — decoding straight off the YAML
            // event stream into a generic value — is a lexical/syntax
            // error, not a semantic one. Signal "not recoverable" so the
            // caller stops rather than continuing to read from a parser
            // left in an indeterminate state.
            let entry = invalid(
                source,
                None,
                InvalidReason::MalformedYaml {
                    message: error.to_string(),
                },
            );
            return (entry, false);
        }
    };

    if let Err(diagnostic) = check_envelope(&value) {
        let entry = CatalogEntry {
            source,
            reference: None,
            identity: None,
            result: LoadResult::Invalid {
                diagnostics: vec![diagnostic],
            },
        };
        return (entry, true);
    }

    let document: CatalogEntryDocument = match serde_path_to_error::deserialize(&value) {
        Ok(document) => document,
        Err(error) => {
            let field_path = Some(error.path().to_string());
            let message = error.into_inner().to_string();
            let entry = CatalogEntry {
                source,
                reference: None,
                identity: None,
                result: LoadResult::Invalid {
                    diagnostics: vec![Diagnostic {
                        field_path,
                        reason: InvalidReason::SchemaMismatch { message },
                    }],
                },
            };
            return (entry, true);
        }
    };

    let identity = build_identity(&document.metadata);

    let entry = match validate_structure(&document) {
        Err(diagnostics) => CatalogEntry {
            source,
            reference: None,
            identity,
            result: LoadResult::Invalid { diagnostics },
        },
        Ok(spec) => {
            let metadata = TemplateMetadata::from_document(&document.metadata);
            let reference = identity.as_ref().map(|identity| {
                global_entry_ref(
                    catalog_root,
                    &source,
                    identity,
                    crate::template_fingerprint(identity, &metadata, &spec),
                )
            });
            match resolve_availability(&spec, component_schemas) {
                AvailabilityOutcome::Available => CatalogEntry {
                    source,
                    reference,
                    identity,
                    result: LoadResult::Available { metadata, spec },
                },
                AvailabilityOutcome::Unavailable(reasons) => CatalogEntry {
                    source,
                    reference,
                    identity,
                    result: LoadResult::Unavailable {
                        metadata,
                        spec,
                        reasons,
                    },
                },
            }
        }
    };
    (entry, true)
}

fn invalid(
    source: SourceLocation,
    field_path: Option<String>,
    reason: InvalidReason,
) -> CatalogEntry {
    CatalogEntry {
        source,
        reference: None,
        identity: None,
        result: LoadResult::Invalid {
            diagnostics: vec![Diagnostic { field_path, reason }],
        },
    }
}

/// `None` when either name fails validation — a best-effort identity for
/// display/duplicate-detection, independent of whether the rest of the
/// entry is later found `Invalid`.
fn build_identity(metadata: &CatalogMetadata) -> Option<TemplateIdentity> {
    let catalog = CatalogScopeName::new(metadata.catalog.clone()).ok()?;
    let template = TemplateName::new(metadata.name.clone()).ok()?;
    Some(TemplateIdentity { catalog, template })
}

/// Decode just `{apiVersion, kind}` from the untyped value and reject before
/// attempting the full typed parse — mirrors `fieldcad-scene-document`'s
/// reject-before-trusting-other-fields discipline, so a document with a
/// wrong `apiVersion` is rejected for that reason even if the rest of its
/// shape is unrecognisable garbage.
fn check_envelope(value: &serde_norway::Value) -> Result<(), Diagnostic> {
    let envelope: CatalogEnvelope =
        serde_path_to_error::deserialize(value).map_err(|error| Diagnostic {
            field_path: Some(error.path().to_string()),
            reason: InvalidReason::SchemaMismatch {
                message: error.into_inner().to_string(),
            },
        })?;

    if envelope.api_version != API_VERSION {
        return Err(Diagnostic {
            field_path: Some("apiVersion".to_owned()),
            reason: InvalidReason::UnsupportedApiVersion {
                found: envelope.api_version,
                expected: API_VERSION.to_owned(),
            },
        });
    }
    if envelope.kind != KIND {
        return Err(Diagnostic {
            field_path: Some("kind".to_owned()),
            reason: InvalidReason::UnsupportedKind {
                found: envelope.kind,
                expected: KIND.to_owned(),
            },
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use fieldcad_sources::{inertial_mass_component_id, inertial_mass_component_schema};
    use std::io::Write;
    use tempfile::TempDir;

    fn registry_with_mass() -> BTreeMap<ComponentTypeId, ComponentSchema> {
        [(
            inertial_mass_component_id(),
            inertial_mass_component_schema(),
        )]
        .into_iter()
        .collect()
    }

    fn write_file(dir: &TempDir, name: &str, contents: &str) -> PathBuf {
        let path = dir.path().join(name);
        let mut file = fs::File::create(&path).unwrap();
        file.write_all(contents.as_bytes()).unwrap();
        path
    }

    const VALID_ENTRY: &str = r#"
apiVersion: fieldcad.catalog/v1
kind: ObjectTemplate
metadata:
  catalog: personal-physics
  name: fancy-unicorn
spec:
  objectKind: world-object
  components:
    - type: { plugin: fieldcad.mass-sources, name: inertial-mass }
      properties:
        mass: { scalar: { siValue: 1.0 } }
"#;

    /// Pinning test for the parser-recovery uncertainty flagged during
    /// design: a *lexical* YAML syntax error (not just a semantic/schema
    /// mismatch) in the middle document of a 3-document stream.
    ///
    /// Empirically confirmed against the vendored `serde_norway` parser
    /// (`Loader::next_document`, `loader.rs`) that a lexical error does
    /// *not* leave the shared parser able to resynchronize to the next
    /// `---` boundary: probing the raw `Deserializer` iterator past the
    /// break kept yielding further (also-erroring) documents rather than
    /// stopping at the stream's actual end. `load_catalog_file` therefore
    /// stops reading a file's stream as soon as a document fails at the
    /// lexical level (see its `recoverable` handling), rather than trusting
    /// per-document isolation the way it can for a purely semantic error
    /// (`a_semantically_broken_middle_document_is_isolated_from_its_siblings`,
    /// below). This test pins that production behaviour: the well-formed
    /// document before the break is preserved, and the file stops there —
    /// so the suite can never hang regardless of how the vendored parser
    /// behaves past the break.
    #[test]
    fn a_lexically_broken_document_stops_the_file_without_hanging() {
        let dir = TempDir::new().unwrap();
        let text = format!("{VALID_ENTRY}\n---\nspec: [unterminated\n---\n{VALID_ENTRY}");
        write_file(&dir, "catalog.yaml", &text);

        let report = load_catalog_directory(dir.path(), &registry_with_mass());

        assert_eq!(
            report.entries.len(),
            2,
            "expected the leading valid document plus the lexically broken one, \
             and nothing read past the break"
        );
        assert!(matches!(
            report.entries[0].result,
            LoadResult::Available { .. }
        ));
        match &report.entries[1].result {
            LoadResult::Invalid { diagnostics } => {
                assert!(matches!(
                    diagnostics[0].reason,
                    InvalidReason::MalformedYaml { .. }
                ));
            }
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn a_semantically_broken_middle_document_is_isolated_from_its_siblings() {
        let dir = TempDir::new().unwrap();
        let text =
            format!("{VALID_ENTRY}\n---\napiVersion: not-a-real-version\n---\n{VALID_ENTRY}");
        write_file(&dir, "catalog.yaml", &text);

        let report = load_catalog_directory(dir.path(), &registry_with_mass());

        assert_eq!(report.entries.len(), 3);
        assert!(matches!(
            report.entries[0].result,
            LoadResult::Available { .. }
        ));
        assert!(matches!(
            report.entries[1].result,
            LoadResult::Invalid { .. }
        ));
        assert_eq!(report.entries[1].source.document_ordinal.get(), 2);
        assert!(matches!(
            report.entries[2].result,
            LoadResult::Available { .. }
        ));
    }

    #[test]
    fn a_file_over_the_size_cap_does_not_block_a_sibling_file() {
        let dir = TempDir::new().unwrap();
        let oversized = "a".repeat(MAX_CATALOG_FILE_BYTES as usize + 1);
        write_file(&dir, "too-big.yaml", &oversized);
        write_file(&dir, "fine.yaml", VALID_ENTRY);

        let report = load_catalog_directory(dir.path(), &registry_with_mass());

        assert_eq!(report.file_errors.len(), 1);
        assert!(matches!(
            report.file_errors[0].reason,
            InvalidReason::FileTooLarge { .. }
        ));
        assert_eq!(report.entries.len(), 1);
        assert!(matches!(
            report.entries[0].result,
            LoadResult::Available { .. }
        ));
    }

    #[test]
    fn wrong_api_version_is_rejected_before_the_spec_is_ever_interpreted() {
        let dir = TempDir::new().unwrap();
        // `spec` here is garbage that would fail a typed parse on its own —
        // proves the apiVersion check happens first.
        let text =
            "apiVersion: fieldcad.catalog/v2\nkind: ObjectTemplate\nmetadata: {}\nspec: 12345\n";
        write_file(&dir, "catalog.yaml", text);

        let report = load_catalog_directory(dir.path(), &registry_with_mass());

        assert_eq!(report.entries.len(), 1);
        match &report.entries[0].result {
            LoadResult::Invalid { diagnostics } => {
                assert!(matches!(
                    diagnostics[0].reason,
                    InvalidReason::UnsupportedApiVersion { .. }
                ));
            }
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn an_entry_with_an_unregistered_component_loads_as_unavailable() {
        let dir = TempDir::new().unwrap();
        write_file(&dir, "catalog.yaml", VALID_ENTRY);

        let report = load_catalog_directory(dir.path(), &BTreeMap::new());

        assert_eq!(report.entries.len(), 1);
        assert!(matches!(
            report.entries[0].result,
            LoadResult::Unavailable { .. }
        ));
    }

    #[test]
    fn an_invalid_entry_still_carries_identity_when_only_metadata_parsed() {
        let dir = TempDir::new().unwrap();
        // Valid metadata, but a shape with a non-positive radius — invalid
        // at the structure tier, not the parse tier.
        let text = r#"
apiVersion: fieldcad.catalog/v1
kind: ObjectTemplate
metadata:
  catalog: personal-physics
  name: fancy-unicorn
spec:
  objectKind: world-object
  shape:
    kind: sphere
    radiusMetres: -1.0
  components: []
"#;
        write_file(&dir, "catalog.yaml", text);

        let report = load_catalog_directory(dir.path(), &registry_with_mass());

        assert_eq!(report.entries.len(), 1);
        assert!(matches!(
            report.entries[0].result,
            LoadResult::Invalid { .. }
        ));
        assert!(report.entries[0].identity.is_some());
    }
}
