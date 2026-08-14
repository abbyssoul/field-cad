//! Atomic YAML writes for disk-based catalog entries.
//!
//! Every function here writes through a temporary file and renames atomically
//! over the target, preserving existing content on crash or partial write.
//! Multi-document YAML streams (one file, many `---`-separated entries) are
//! fully supported: editing one entry preserves all other documents.
//!
//! Catalog entry display values are constructed as [`serde_norway::Value`]
//! trees rather than serialised through the document DTOs — the DTOs have
//! hand-written `Deserialize` for [`crate::CatalogPropertyValue`] whose
//! default `Serialize` would not round-trip — so fields are named by hand
//! to match the existing camelCase YAML wire format.

use fieldcad_core::quantities::SiScalar;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use uuid::Uuid;

use crate::structure::{
    TemplateComponentInstance, TemplatePropertyValue, TemplateShape, TemplateSpec,
};

/// Captured file metadata used to detect external modifications between load
/// and write — see [`save_entry`]'s `conflict_check` parameter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FingerprintState {
    /// SHA-256 of the complete source bytes. This is the conflict authority;
    /// timestamps are intentionally not trusted because editors can preserve
    /// them or update more quickly than a filesystem's resolution.
    pub digest: [u8; 32],
}

/// Complete eligible-file snapshot for hot reload. Unreadable files are kept
/// in the snapshot too, so a permission or I/O change triggers a reload.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DirectoryState {
    pub files: std::collections::BTreeMap<PathBuf, Result<FingerprintState, String>>,
}

#[derive(Debug, thiserror::Error)]
pub enum WriteError {
    #[error("{0}")]
    Io(#[source] std::io::Error),
    #[error("YAML serialisation failed: {0}")]
    Serialize(String),
    #[error("catalog file changed since it was loaded: {0}")]
    SourceModified(PathBuf),
    #[error("file is read-only: {0}")]
    ReadOnly(PathBuf),
    #[error("entry '{identity}' not found in {path}")]
    EntryNotFound { identity: String, path: PathBuf },
    #[error("refusing to overwrite existing catalog source: {0}")]
    SourceAlreadyExists(PathBuf),
    #[error("cannot parse catalog file as a YAML stream: {path}")]
    StreamParse {
        path: PathBuf,
        #[source]
        source: serde_norway::Error,
    },
    #[error("{0}")]
    Other(String),
}

/// Exact YAML document selected for an edit/remove. Identity is verified at
/// the ordinal after the digest check so same-named documents never alias.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceTarget {
    pub path: PathBuf,
    pub document_ordinal: crate::DocumentOrdinal,
    pub identity: crate::TemplateIdentity,
}

/// Stat a file and capture the modification timestamp used by
/// [`save_entry`]'s conflict check.
pub fn file_state(path: &Path) -> Result<Option<FingerprintState>, std::io::Error> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(FingerprintState {
            digest: Sha256::digest(bytes).into(),
        })),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Snapshot every eligible YAML file in `dir`, including files that cannot be
/// read. A flat scan matches the catalog loader's scope.
pub fn directory_state(dir: &Path) -> DirectoryState {
    let mut state = DirectoryState::default();
    let Ok(entries) = fs::read_dir(dir) else {
        return state;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let yaml = path
            .extension()
            .and_then(|value| value.to_str())
            .is_some_and(|value| {
                value.eq_ignore_ascii_case("yaml") || value.eq_ignore_ascii_case("yml")
            });
        if yaml {
            let result = match file_state(&path) {
                Ok(Some(state)) => Ok(state),
                Ok(None) => Err("file disappeared during catalog scan".to_owned()),
                Err(error) => Err(error.to_string()),
            };
            state.files.insert(path, result);
        }
    }
    state
}

fn check_read_only(path: &Path) -> Result<(), WriteError> {
    if let Ok(meta) = fs::metadata(path)
        && meta.permissions().readonly()
    {
        return Err(WriteError::ReadOnly(path.to_path_buf()));
    }
    Ok(())
}

fn check_conflict(path: &Path, expected: &FingerprintState) -> Result<(), WriteError> {
    if file_state(path).map_err(WriteError::Io)? != Some(expected.clone()) {
        return Err(WriteError::SourceModified(path.to_path_buf()));
    }
    Ok(())
}

/// Replace exactly one source document selected during catalog load, retaining
/// its source position while allowing its catalog/template identity to change.
pub fn save_entry_at(
    target: &SourceTarget,
    replacement_identity: &crate::TemplateIdentity,
    metadata: &crate::TemplateMetadata,
    spec: &TemplateSpec,
    expected: &FingerprintState,
) -> Result<(), WriteError> {
    check_read_only(&target.path)?;
    check_conflict(&target.path, expected)?;
    let mut docs = read_stream(&target.path)?;
    let index = target.document_ordinal.get().saturating_sub(1);
    let Some(document) = docs.get(index) else {
        return Err(WriteError::EntryNotFound {
            identity: target.identity.to_string(),
            path: target.path.clone(),
        });
    };
    if !document_matches_identity(document, &target.identity) {
        return Err(WriteError::EntryNotFound {
            identity: target.identity.to_string(),
            path: target.path.clone(),
        });
    }
    docs[index] = entry_to_yaml(replacement_identity, metadata, spec);
    write_stream(&target.path, &docs)
}

/// Remove exactly one source document selected during catalog load.
pub fn remove_entry_at(
    target: &SourceTarget,
    expected: &FingerprintState,
) -> Result<(), WriteError> {
    check_read_only(&target.path)?;
    check_conflict(&target.path, expected)?;
    let mut docs = read_stream(&target.path)?;
    let index = target.document_ordinal.get().saturating_sub(1);
    if index >= docs.len() || !document_matches_identity(&docs[index], &target.identity) {
        return Err(WriteError::EntryNotFound {
            identity: target.identity.to_string(),
            path: target.path.clone(),
        });
    }
    docs.remove(index);
    if docs.is_empty() {
        fs::remove_file(&target.path).map_err(WriteError::Io)
    } else {
        write_stream(&target.path, &docs)
    }
}

/// Atomically create a new, single-document catalog source. Existing files
/// are never reused by this API; edits must use [`save_entry_at`].
pub fn create_entry(
    path: &Path,
    identity: &crate::TemplateIdentity,
    metadata: &crate::TemplateMetadata,
    spec: &TemplateSpec,
) -> Result<(), WriteError> {
    if path.exists() {
        return Err(WriteError::SourceAlreadyExists(path.to_path_buf()));
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(WriteError::Io)?;
    }
    write_stream(path, &[entry_to_yaml(identity, metadata, spec)])
}

fn document_matches_identity(
    document: &serde_norway::Value,
    identity: &crate::TemplateIdentity,
) -> bool {
    document
        .get("metadata")
        .and_then(|value| value.get("catalog"))
        .and_then(|value| value.as_str())
        == Some(identity.catalog.as_str())
        && document
            .get("metadata")
            .and_then(|value| value.get("name"))
            .and_then(|value| value.as_str())
            == Some(identity.template.as_str())
}

/// Atomic write of one catalog entry to its YAML source file.
///
/// If the file already exists it is re-read as a YAML stream; the document
/// whose `metadata.catalog` and `metadata.name` match `identity` is replaced
/// by `(identity, metadata, spec)`, and all documents (modified or not) are
/// written back separated by `---`.  A file missing from disk is created with
/// a single entry.
///
/// When `conflict_check` is `Some`, the file's modification time is compared
/// against the captured state before any write begins — returning
/// [`WriteError::SourceModified`] on mismatch rather than overwriting a
/// hand-edited file.
#[cfg(test)]
fn save_entry(
    path: &Path,
    identity: &crate::TemplateIdentity,
    metadata: &crate::TemplateMetadata,
    spec: &TemplateSpec,
    conflict_check: Option<&FingerprintState>,
) -> Result<(), WriteError> {
    check_read_only(path)?;

    if let Some(expected) = conflict_check {
        check_conflict(path, expected)?;
    }

    let documents = if path.exists() {
        let mut docs = read_stream(path)?;
        let target_catalog = identity.catalog.as_str();
        let target_name = identity.template.as_str();

        let pos = docs.iter().position(|doc| {
            doc.get("metadata")
                .and_then(|m| m.get("catalog"))
                .and_then(|v| v.as_str())
                == Some(target_catalog)
                && doc
                    .get("metadata")
                    .and_then(|m| m.get("name"))
                    .and_then(|v| v.as_str())
                    == Some(target_name)
        });

        match pos {
            Some(p) => docs[p] = entry_to_yaml(identity, metadata, spec),
            None => docs.push(entry_to_yaml(identity, metadata, spec)),
        }
        docs
    } else {
        vec![entry_to_yaml(identity, metadata, spec)]
    };

    write_stream(path, &documents)
}

/// Remove a single entry from a multi-document YAML catalog file, keeping
/// every other entry.
///
/// If the file is reduced to zero documents it is deleted rather than left
/// as an empty file — an absent file is the same as an empty catalog.
#[cfg(test)]
fn remove_entry(
    path: &Path,
    identity: &crate::TemplateIdentity,
    conflict_check: Option<&FingerprintState>,
) -> Result<(), WriteError> {
    check_read_only(path)?;
    if let Some(expected) = conflict_check {
        check_conflict(path, expected)?;
    }
    if !path.exists() {
        return Ok(());
    }

    let mut docs = read_stream(path)?;
    let target_catalog = identity.catalog.as_str();
    let target_name = identity.template.as_str();

    let before = docs.len();
    docs.retain(|doc| {
        !(doc
            .get("metadata")
            .and_then(|m| m.get("catalog"))
            .and_then(|v| v.as_str())
            == Some(target_catalog)
            && doc
                .get("metadata")
                .and_then(|m| m.get("name"))
                .and_then(|v| v.as_str())
                == Some(target_name))
    });

    if docs.len() == before {
        return Err(WriteError::EntryNotFound {
            identity: format!("{target_catalog}/{target_name}"),
            path: path.to_path_buf(),
        });
    }

    if docs.is_empty() {
        return fs::remove_file(path).map_err(WriteError::Io);
    }

    write_stream(path, &docs)
}

fn read_stream(path: &Path) -> Result<Vec<serde_norway::Value>, WriteError> {
    let text = fs::read_to_string(path).map_err(WriteError::Io)?;
    let mut documents = Vec::new();
    for document in serde_norway::Deserializer::from_str(&text) {
        let value =
            serde_norway::Value::deserialize(document).map_err(|e| WriteError::StreamParse {
                path: path.to_path_buf(),
                source: e,
            })?;
        documents.push(value);
    }
    Ok(documents)
}

fn write_stream(path: &Path, documents: &[serde_norway::Value]) -> Result<(), WriteError> {
    let mut buffer = String::new();
    for document in documents {
        buffer.push_str("---\n");
        let yaml =
            serde_norway::to_string(document).map_err(|e| WriteError::Serialize(e.to_string()))?;
        buffer.push_str(&yaml);
    }
    if documents.is_empty() {
        buffer.push_str("---\n");
    }

    let tmp = tmp_path(path);
    {
        let mut file = fs::File::create(&tmp).map_err(WriteError::Io)?;
        file.write_all(buffer.as_bytes()).map_err(WriteError::Io)?;
        file.sync_all().map_err(WriteError::Io)?;
    }
    replace_file(&tmp, path)?;
    Ok(())
}

#[cfg(not(windows))]
fn replace_file(tmp: &Path, path: &Path) -> Result<(), WriteError> {
    fs::rename(tmp, path).map_err(WriteError::Io)?;
    if let Some(parent) = path.parent()
        && let Ok(directory) = fs::File::open(parent)
    {
        let _ = directory.sync_all();
    }
    Ok(())
}

/// Windows does not replace an existing destination with `rename`. Preserve a
/// verified backup before its best-effort remove-and-rename fallback; callers
/// get an error rather than a silent partial success if the final replace fails.
#[cfg(windows)]
fn replace_file(tmp: &Path, path: &Path) -> Result<(), WriteError> {
    if !path.exists() {
        return fs::rename(tmp, path).map_err(WriteError::Io);
    }
    let backup = path.with_extension("yaml.bak");
    fs::copy(path, &backup).map_err(WriteError::Io)?;
    fs::remove_file(path).map_err(WriteError::Io)?;
    fs::rename(tmp, path).map_err(WriteError::Io)
}

fn tmp_path(path: &Path) -> PathBuf {
    let stem = path
        .file_stem()
        .map(|s| s.to_os_string())
        .unwrap_or_default();
    let name = if let Some(ext) = path.extension() {
        let mut n = stem;
        n.push(".");
        n.push(ext);
        n.push(format!(".{}.tmp", Uuid::new_v4()));
        n
    } else {
        let mut n = stem;
        n.push(format!(".{}.tmp", Uuid::new_v4()));
        n
    };
    path.with_file_name(name)
}

// --- YAML construction helpers ---

fn yaml_string(value: &str) -> serde_norway::Value {
    serde_norway::Value::String(value.to_owned())
}

fn yaml_f64(value: f64) -> serde_norway::Value {
    serde_norway::to_value(value).expect("finite f64 always serializable")
}

fn yaml_bool(flag: bool) -> serde_norway::Value {
    serde_norway::Value::Bool(flag)
}

fn yaml_array_f64(values: &[f64]) -> serde_norway::Value {
    serde_norway::Value::Sequence(values.iter().map(|v| yaml_f64(*v)).collect())
}

fn yaml_map(items: Vec<(&str, serde_norway::Value)>) -> serde_norway::Value {
    let mut mapping = serde_norway::Mapping::new();
    for (key, value) in items {
        mapping.insert(yaml_string(key), value);
    }
    serde_norway::Value::Mapping(mapping)
}

fn entry_to_yaml(
    identity: &crate::TemplateIdentity,
    metadata: &crate::TemplateMetadata,
    spec: &TemplateSpec,
) -> serde_norway::Value {
    yaml_map(vec![
        ("apiVersion", yaml_string(crate::document::API_VERSION)),
        ("kind", yaml_string(crate::document::KIND)),
        ("metadata", metadata_to_yaml(identity, metadata)),
        ("spec", spec_to_yaml(spec)),
    ])
}

fn metadata_to_yaml(
    identity: &crate::TemplateIdentity,
    metadata: &crate::TemplateMetadata,
) -> serde_norway::Value {
    let mut items = vec![
        ("catalog", yaml_string(identity.catalog.as_str())),
        ("name", yaml_string(identity.template.as_str())),
    ];

    if let Some(desc) = &metadata.description
        && !desc.is_empty()
    {
        items.push(("description", yaml_string(desc)));
    }
    if let Some(author) = &metadata.author
        && !author.is_empty()
    {
        items.push(("author", yaml_string(author)));
    }
    if !metadata.labels.is_empty() {
        let mut map = serde_norway::Mapping::new();
        for (k, v) in &metadata.labels {
            map.insert(yaml_string(k), yaml_string(v));
        }
        items.push(("labels", serde_norway::Value::Mapping(map)));
    }
    if !metadata.annotations.is_empty() {
        let mut map = serde_norway::Mapping::new();
        for (k, v) in &metadata.annotations {
            map.insert(yaml_string(k), yaml_string(v));
        }
        items.push(("annotations", serde_norway::Value::Mapping(map)));
    }

    yaml_map(items)
}

fn spec_to_yaml(spec: &TemplateSpec) -> serde_norway::Value {
    let mut items = vec![("objectKind", yaml_string(&spec.object_kind))];

    if let Some(shape) = &spec.shape {
        items.push(("shape", shape_to_yaml(shape)));
    }
    if !spec.components.is_empty() {
        let components: Vec<_> = spec.components.iter().map(component_to_yaml).collect();
        items.push(("components", serde_norway::Value::Sequence(components)));
    }

    yaml_map(items)
}

fn shape_to_yaml(shape: &TemplateShape) -> serde_norway::Value {
    let mut items = vec![];

    match shape {
        TemplateShape::Point { exclusion_radius } => {
            items.push(("kind", yaml_string("point")));
            items.push((
                "exclusionRadiusMetres",
                yaml_f64(exclusion_radius.into_si()),
            ));
        }
        TemplateShape::Sphere { radius } => {
            items.push(("kind", yaml_string("sphere")));
            items.push(("radiusMetres", yaml_f64(radius.into_si())));
        }
        TemplateShape::Box { half_extent } => {
            items.push(("kind", yaml_string("box")));
            items.push((
                "halfExtentMetres",
                yaml_array_f64(&[half_extent.x, half_extent.y, half_extent.z]),
            ));
        }
    }

    yaml_map(items)
}

fn component_to_yaml(component: &TemplateComponentInstance) -> serde_norway::Value {
    let type_ref = component_type_to_yaml(&component.component_type);

    let mut items = vec![("type", type_ref)];

    if !component.properties.is_empty() {
        let mut props = serde_norway::Mapping::new();
        for (prop_id, value) in &component.properties {
            props.insert(yaml_string(prop_id.as_str()), property_value_to_yaml(value));
        }
        items.push(("properties", serde_norway::Value::Mapping(props)));
    }

    yaml_map(items)
}

fn component_type_to_yaml(component_type: &fieldcad_core::ComponentTypeId) -> serde_norway::Value {
    let mut map = serde_norway::Mapping::new();
    map.insert(
        yaml_string("plugin"),
        yaml_string(&component_type.plugin().to_string()),
    );
    map.insert(yaml_string("name"), yaml_string(component_type.name()));
    serde_norway::Value::Mapping(map)
}

fn property_value_to_yaml(value: &TemplatePropertyValue) -> serde_norway::Value {
    match value {
        TemplatePropertyValue::Scalar { si_value } => yaml_map(vec![(
            "scalar",
            yaml_map(vec![("siValue", yaml_f64(*si_value))]),
        )]),
        TemplatePropertyValue::Vector { si_value } => yaml_map(vec![(
            "vector",
            yaml_map(vec![(
                "siValue",
                yaml_array_f64(&[si_value.x, si_value.y, si_value.z]),
            )]),
        )]),
        TemplatePropertyValue::Boolean(flag) => yaml_map(vec![("boolean", yaml_bool(*flag))]),
        TemplatePropertyValue::Text(text) => yaml_map(vec![("text", yaml_string(text))]),
        TemplatePropertyValue::Choice(choice) => yaml_map(vec![("choice", yaml_string(choice))]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TemplateIdentity;
    use crate::ids::{CatalogScopeName, TemplateName};
    use crate::structure::TemplatePropertyValue;
    use fieldcad_core::quantities::LengthMetres;
    use fieldcad_core::{ComponentTypeId, PluginId, PropertyId};
    use std::collections::BTreeMap;

    fn identity() -> TemplateIdentity {
        TemplateIdentity {
            catalog: CatalogScopeName::new("test").unwrap(),
            template: TemplateName::new("test-entry").unwrap(),
        }
    }

    fn metadata() -> crate::TemplateMetadata {
        crate::TemplateMetadata {
            description: Some("A test entry".to_owned()),
            author: Some("Test Author".to_owned()),
            labels: BTreeMap::from([("topic".to_owned(), "testing".to_owned())]),
            annotations: BTreeMap::new(),
        }
    }

    fn point_spec() -> TemplateSpec {
        let component_type =
            ComponentTypeId::new(PluginId::new("fieldcad.sources").unwrap(), "inertial-mass")
                .unwrap();
        let mut props = BTreeMap::new();
        props.insert(
            PropertyId::new("mass").unwrap(),
            TemplatePropertyValue::Scalar { si_value: 1.0 },
        );
        TemplateSpec {
            object_kind: "world-object".to_owned(),
            shape: Some(TemplateShape::Point {
                exclusion_radius: LengthMetres::from_si(0.15),
            }),
            components: vec![TemplateComponentInstance {
                component_type,
                properties: props,
            }],
        }
    }

    fn no_shape_spec() -> TemplateSpec {
        let mut s = point_spec();
        s.shape = None;
        s
    }

    #[test]
    fn save_and_load_round_trips_single_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("single.yaml");

        save_entry(&path, &identity(), &metadata(), &no_shape_spec(), None).unwrap();
        assert!(path.exists());

        let bytes = fs::read_to_string(&path).unwrap();
        assert!(bytes.starts_with("---\n"));

        let mut report = crate::CatalogLoadReport::default();
        crate::load::load_catalog_file(&path, &BTreeMap::new(), &mut report);
        assert_eq!(report.entries.len(), 1);
        assert!(
            matches!(
                report.entries[0].result,
                crate::LoadResult::Available { .. } | crate::LoadResult::Unavailable { .. }
            ),
            "entry must be parsable"
        );
    }

    #[test]
    fn save_entry_updates_middle_document_in_stream() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("multi.yaml");

        let a = TemplateIdentity {
            catalog: CatalogScopeName::new("test").unwrap(),
            template: TemplateName::new("alpha").unwrap(),
        };
        let b = TemplateIdentity {
            catalog: CatalogScopeName::new("test").unwrap(),
            template: TemplateName::new("beta").unwrap(),
        };
        let c = TemplateIdentity {
            catalog: CatalogScopeName::new("test").unwrap(),
            template: TemplateName::new("gamma").unwrap(),
        };

        save_entry(&path, &a, &metadata(), &no_shape_spec(), None).unwrap();
        save_entry(&path, &b, &metadata(), &no_shape_spec(), None).unwrap();
        save_entry(&path, &c, &metadata(), &no_shape_spec(), None).unwrap();

        let mut updated = metadata();
        updated.description = Some("Updated beta".to_owned());
        save_entry(&path, &b, &updated, &no_shape_spec(), None).unwrap();

        let bytes = fs::read_to_string(&path).unwrap();
        assert!(bytes.contains("Updated beta"));
        assert!(bytes.contains("A test entry"));

        let docs = read_stream(&path).unwrap();
        assert_eq!(docs.len(), 3);
    }

    #[test]
    fn conflict_detection_rejects_modified_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("conflict.yaml");

        save_entry(&path, &identity(), &metadata(), &no_shape_spec(), None).unwrap();
        let state = file_state(&path).unwrap().unwrap();

        // Sleep so the filesystem mtime actually changes
        std::thread::sleep(std::time::Duration::from_millis(10));
        fs::write(&path, b"---\n").unwrap();

        let err = save_entry(
            &path,
            &identity(),
            &metadata(),
            &no_shape_spec(),
            Some(&state),
        )
        .unwrap_err();
        assert!(matches!(err, WriteError::SourceModified(_)));
    }

    #[test]
    fn remove_entry_keeps_other_documents() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("multi.yaml");

        let a = TemplateIdentity {
            catalog: CatalogScopeName::new("test").unwrap(),
            template: TemplateName::new("alpha").unwrap(),
        };
        let b = TemplateIdentity {
            catalog: CatalogScopeName::new("test").unwrap(),
            template: TemplateName::new("beta").unwrap(),
        };

        save_entry(&path, &a, &metadata(), &no_shape_spec(), None).unwrap();
        save_entry(&path, &b, &metadata(), &no_shape_spec(), None).unwrap();

        remove_entry(&path, &a, None).unwrap();

        let docs = read_stream(&path).unwrap();
        assert_eq!(docs.len(), 1);
        assert_eq!(
            docs[0]
                .get("metadata")
                .and_then(|m| m.get("name"))
                .and_then(|v| v.as_str()),
            Some("beta")
        );
    }

    #[test]
    fn removing_last_entry_deletes_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("single.yaml");

        save_entry(&path, &identity(), &metadata(), &no_shape_spec(), None).unwrap();
        remove_entry(&path, &identity(), None).unwrap();

        assert!(!path.exists());
    }

    #[test]
    fn read_only_file_is_rejected_on_save() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("readonly.yaml");

        save_entry(&path, &identity(), &metadata(), &no_shape_spec(), None).unwrap();

        let mut perms = fs::metadata(&path).unwrap().permissions();
        perms.set_readonly(true);
        fs::set_permissions(&path, perms).unwrap();

        let err = save_entry(&path, &identity(), &metadata(), &no_shape_spec(), None).unwrap_err();
        assert!(matches!(err, WriteError::ReadOnly(_)));
    }

    #[test]
    fn broken_yaml_is_rejected_as_stream_parse() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("broken.yaml");
        fs::write(&path, "not: valid: yaml: [[").unwrap();

        let err = save_entry(&path, &identity(), &metadata(), &no_shape_spec(), None).unwrap_err();
        assert!(matches!(err, WriteError::StreamParse { .. }));
    }

    #[test]
    fn save_entry_creates_new_file_with_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent.yaml");

        assert!(!path.exists());
        save_entry(&path, &identity(), &metadata(), &no_shape_spec(), None).unwrap();

        let docs = read_stream(&path).unwrap();
        assert_eq!(docs.len(), 1);
        assert_eq!(
            docs[0]
                .get("metadata")
                .and_then(|m| m.get("name"))
                .and_then(|v| v.as_str()),
            Some("test-entry")
        );
    }

    #[test]
    fn source_qualified_edit_updates_only_the_selected_duplicate_document() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("duplicates.yaml");
        let identity = identity();
        let mut first = metadata();
        first.description = Some("first".to_owned());
        let mut second = metadata();
        second.description = Some("second".to_owned());
        write_stream(
            &path,
            &[
                entry_to_yaml(&identity, &first, &no_shape_spec()),
                entry_to_yaml(&identity, &second, &no_shape_spec()),
            ],
        )
        .unwrap();
        let expected = file_state(&path).unwrap().unwrap();
        let mut replacement = metadata();
        replacement.description = Some("replacement".to_owned());
        save_entry_at(
            &SourceTarget {
                path: path.clone(),
                document_ordinal: crate::DocumentOrdinal::new(1),
                identity: identity.clone(),
            },
            &identity,
            &replacement,
            &no_shape_spec(),
            &expected,
        )
        .unwrap();
        let text = fs::read_to_string(path).unwrap();
        assert!(text.contains("first"));
        assert!(text.contains("replacement"));
        assert!(!text.contains("second"));
    }

    #[test]
    fn source_qualified_edit_can_rename_a_document_without_moving_its_siblings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("multi.yaml");
        let alpha = TemplateIdentity {
            catalog: CatalogScopeName::new("test").unwrap(),
            template: TemplateName::new("alpha").unwrap(),
        };
        let beta = TemplateIdentity {
            catalog: CatalogScopeName::new("test").unwrap(),
            template: TemplateName::new("beta").unwrap(),
        };
        let renamed = TemplateIdentity {
            catalog: CatalogScopeName::new("renamed").unwrap(),
            template: TemplateName::new("gamma").unwrap(),
        };
        write_stream(
            &path,
            &[
                entry_to_yaml(&alpha, &metadata(), &no_shape_spec()),
                entry_to_yaml(&beta, &metadata(), &no_shape_spec()),
            ],
        )
        .unwrap();
        let expected = file_state(&path).unwrap().unwrap();
        save_entry_at(
            &SourceTarget {
                path: path.clone(),
                document_ordinal: crate::DocumentOrdinal::new(1),
                identity: beta,
            },
            &renamed,
            &metadata(),
            &no_shape_spec(),
            &expected,
        )
        .unwrap();
        let docs = read_stream(&path).unwrap();
        assert_eq!(docs.len(), 2);
        assert_eq!(
            docs[0]
                .get("metadata")
                .and_then(|m| m.get("name"))
                .and_then(|v| v.as_str()),
            Some("alpha")
        );
        assert_eq!(
            docs[1]
                .get("metadata")
                .and_then(|m| m.get("catalog"))
                .and_then(|v| v.as_str()),
            Some("renamed")
        );
        assert_eq!(
            docs[1]
                .get("metadata")
                .and_then(|m| m.get("name"))
                .and_then(|v| v.as_str()),
            Some("gamma")
        );
    }

    #[test]
    fn digest_conflict_rejects_a_rewrite_without_relying_on_mtime() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("conflict.yaml");
        save_entry(&path, &identity(), &metadata(), &no_shape_spec(), None).unwrap();
        let expected = file_state(&path).unwrap().unwrap();
        fs::write(&path, "---\nchanged: externally\n").unwrap();
        let error = save_entry_at(
            &SourceTarget {
                path: path.clone(),
                document_ordinal: crate::DocumentOrdinal::new(0),
                identity: identity(),
            },
            &identity(),
            &metadata(),
            &no_shape_spec(),
            &expected,
        )
        .unwrap_err();
        assert!(matches!(error, WriteError::SourceModified(found) if found == path));
    }

    #[test]
    fn source_qualified_remove_uses_the_same_digest_conflict_check() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("remove.yaml");
        save_entry(&path, &identity(), &metadata(), &no_shape_spec(), None).unwrap();
        let expected = file_state(&path).unwrap().unwrap();
        fs::write(&path, "---\nchanged: externally\n").unwrap();
        let error = remove_entry_at(
            &SourceTarget {
                path: path.clone(),
                document_ordinal: crate::DocumentOrdinal::new(0),
                identity: identity(),
            },
            &expected,
        )
        .unwrap_err();
        assert!(matches!(error, WriteError::SourceModified(found) if found == path));
    }

    #[test]
    fn directory_snapshot_detects_added_and_removed_yaml_files() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first.yaml");
        fs::write(&first, "---\n").unwrap();
        let before = directory_state(dir.path());
        let second = dir.path().join("second.yml");
        fs::write(&second, "---\n").unwrap();
        assert_ne!(before, directory_state(dir.path()));
        fs::remove_file(&first).unwrap();
        assert_ne!(before, directory_state(dir.path()));
    }
}
