//! Raw YAML DTOs for a catalog entry document.
//!
//! Wire spelling is deliberately camelCase, matching the task's illustrative
//! `apiVersion`/`objectKind`/`siValue` example — a scoped deviation from the
//! rest of the codebase's snake_case, because this is the first
//! hand-authored, end-user-facing document format rather than an internal
//! wire protocol.
//!
//! These types only decode the document's shape. They do not validate
//! identifiers, geometry, or finiteness — see [`crate::structure`] for that.

use std::collections::BTreeMap;

use serde::Deserialize;

pub const API_VERSION: &str = "fieldcad.catalog/v1";
pub const KIND: &str = "ObjectTemplate";

/// The `apiVersion`/`kind` discriminator, decoded on its own before the rest
/// of a document is trusted — mirrors `fieldcad-scene-document`'s
/// reject-before-trusting-other-fields discipline.
#[derive(Clone, Debug, Deserialize)]
pub struct CatalogEnvelope {
    #[serde(rename = "apiVersion")]
    pub api_version: String,
    pub kind: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogEntryDocument {
    #[serde(rename = "apiVersion")]
    pub api_version: String,
    pub kind: String,
    pub metadata: CatalogMetadata,
    pub spec: CatalogSpec,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogMetadata {
    pub catalog: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    /// Free-form provenance (e.g. "imported-from"), kept separate from
    /// `labels` (searchable tags).
    #[serde(default)]
    pub annotations: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogSpec {
    /// Deliberately a raw string, not a closed enum: an unrecognised value
    /// (e.g. a future "source"/"emitter" kind) must remain a *parsable*
    /// entry, only unavailable, never an `Invalid` parse failure.
    pub object_kind: String,
    #[serde(default)]
    pub shape: Option<CatalogShape>,
    /// A one-time seed for a newly-instantiated object's display color —
    /// see [`crate::structure::TemplateSpec::default_color`]. Never read
    /// back once instantiated: an object's color is free from that moment
    /// on, exactly like its name.
    #[serde(default)]
    pub default_color: Option<CatalogColor>,
    #[serde(default)]
    pub components: Vec<CatalogComponentInstance>,
}

/// `f64` for consistency with [`CatalogShape`]'s wire fields, narrowed to
/// `f32` (the renderer's native precision) during [`crate::structure::validate_structure`].
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogColor {
    pub r: f64,
    pub g: f64,
    pub b: f64,
    pub a: f64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum CatalogShape {
    Point { exclusion_radius_metres: f64 },
    Sphere { radius_metres: f64 },
    Box { half_extent_metres: [f64; 3] },
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogComponentInstance {
    #[serde(rename = "type")]
    pub component_type: CatalogComponentTypeRef,
    #[serde(default)]
    pub properties: BTreeMap<String, CatalogPropertyValue>,
}

/// `{ plugin: "fieldcad.mass-sources", name: "inertial-mass" }`.
///
/// Deliberately not `fieldcad_core::ComponentTypeId` directly:
/// `ComponentTypeId`'s `Deserialize` expects a single `"plugin:name"`
/// *string* (its hand-written impl over a private `plugin:name` separator),
/// not a two-field YAML mapping. Convert via `ComponentTypeId::new` in
/// [`crate::structure`].
#[derive(Clone, Debug, Deserialize)]
pub struct CatalogComponentTypeRef {
    pub plugin: String,
    pub name: String,
}

/// The wire form of a property value: `{ scalar: { siValue: 1.0 } }`.
///
/// Deliberately undimensioned: `fieldcad_core::PropertyValue::Scalar`
/// bakes its `Dimension` into the value itself, which duplicates exactly
/// the field the schema already declares. The dimension is filled in from
/// the resolved `PropertySchema` during availability resolution, never read
/// from YAML.
///
/// `Deserialize` is hand-written rather than derived: serde's default
/// externally-tagged representation (`{ scalar: { siValue: 1.0 } }`) is a
/// JSON/serde_json convention. `serde_yaml`/`serde_norway` only accept an
/// externally-tagged struct/newtype variant via an explicit YAML `!tag`
/// (e.g. `!scalar { siValue: 1.0 }`), never the map-keyed form — confirmed
/// against the vendored parser (`deserialize_enum` in `de.rs` errors "a
/// YAML tag starting with '!'" for a plain mapping). Visiting the map
/// directly, keyed by variant name, sidesteps that entirely and preserves
/// the task doc's illustrative wire spelling.
#[derive(Clone, Debug)]
pub enum CatalogPropertyValue {
    Scalar { si_value: f64 },
    Vector { si_value: [f64; 3] },
    Boolean(bool),
    Text(String),
    Choice(String),
}

impl<'de> Deserialize<'de> for CatalogPropertyValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct PropertyValueVisitor;

        impl<'de> serde::de::Visitor<'de> for PropertyValueVisitor {
            type Value = CatalogPropertyValue;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str(
                    "a map with exactly one key: scalar, vector, boolean, text, or choice",
                )
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::MapAccess<'de>,
            {
                #[derive(Deserialize)]
                #[serde(rename_all = "camelCase")]
                struct ScalarContent {
                    si_value: f64,
                }
                #[derive(Deserialize)]
                #[serde(rename_all = "camelCase")]
                struct VectorContent {
                    si_value: [f64; 3],
                }

                let key: String = map
                    .next_key()?
                    .ok_or_else(|| serde::de::Error::custom("expected exactly one key"))?;
                let value = match key.as_str() {
                    "scalar" => {
                        let content: ScalarContent = map.next_value()?;
                        CatalogPropertyValue::Scalar {
                            si_value: content.si_value,
                        }
                    }
                    "vector" => {
                        let content: VectorContent = map.next_value()?;
                        CatalogPropertyValue::Vector {
                            si_value: content.si_value,
                        }
                    }
                    "boolean" => CatalogPropertyValue::Boolean(map.next_value()?),
                    "text" => CatalogPropertyValue::Text(map.next_value()?),
                    "choice" => CatalogPropertyValue::Choice(map.next_value()?),
                    other => {
                        return Err(serde::de::Error::unknown_variant(
                            other,
                            &["scalar", "vector", "boolean", "text", "choice"],
                        ));
                    }
                };

                if map.next_key::<String>()?.is_some() {
                    return Err(serde::de::Error::custom("expected exactly one key"));
                }

                Ok(value)
            }
        }

        deserializer.deserialize_map(PropertyValueVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The task doc's own illustrative YAML, verbatim, catches drift between
    /// this module's field spellings and the doc's own example.
    const ILLUSTRATIVE_YAML: &str = r#"
apiVersion: fieldcad.catalog/v1
kind: ObjectTemplate
metadata:
  catalog: personal-physics
  name: fancy-unicorn
  labels:
    topic: demonstration
spec:
  objectKind: world-object
  shape:
    kind: point
    exclusionRadiusMetres: 0.15
  components:
    - type: { plugin: fieldcad.mass-sources, name: inertial-mass }
      properties:
        mass: { scalar: { siValue: 1.0 } }
"#;

    #[test]
    fn parses_the_task_docs_illustrative_example() {
        let document: CatalogEntryDocument = serde_norway::from_str(ILLUSTRATIVE_YAML).unwrap();

        assert_eq!(document.api_version, API_VERSION);
        assert_eq!(document.kind, KIND);
        assert_eq!(document.metadata.catalog, "personal-physics");
        assert_eq!(document.metadata.name, "fancy-unicorn");
        assert_eq!(
            document.metadata.labels.get("topic").map(String::as_str),
            Some("demonstration")
        );
        assert_eq!(document.spec.object_kind, "world-object");
        assert!(matches!(
            document.spec.shape,
            Some(CatalogShape::Point {
                exclusion_radius_metres
            }) if exclusion_radius_metres == 0.15
        ));
        assert_eq!(document.spec.components.len(), 1);
        let component = &document.spec.components[0];
        assert_eq!(component.component_type.plugin, "fieldcad.mass-sources");
        assert_eq!(component.component_type.name, "inertial-mass");
        assert!(matches!(
            component.properties.get("mass"),
            Some(CatalogPropertyValue::Scalar { si_value }) if *si_value == 1.0
        ));
    }
}
