//! Catalog/template name newtypes.
//!
//! These are user-authored labels, not [`fieldcad_core`] world identifiers,
//! but a source location plus catalog+template name identifies a live link
//! within an installed catalog, so they need the same stable, unambiguous
//! character set `fieldcad_core`'s identifiers use. That predicate is a
//! private free function over there (`fieldcad_core::ids::validate_identifier`),
//! not exported, so it is reimplemented here rather than reused.

use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum NameError {
    #[error("name cannot be empty")]
    Empty,
    #[error("name '{value}' contains unsupported characters")]
    InvalidCharacters { value: String },
}

fn validate_name(value: &str) -> Result<(), NameError> {
    if value.is_empty() {
        return Err(NameError::Empty);
    }
    if !value
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.'))
    {
        return Err(NameError::InvalidCharacters {
            value: value.to_owned(),
        });
    }
    Ok(())
}

macro_rules! catalog_name {
    ($name:ident) => {
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, NameError> {
                let value = value.into();
                validate_name(&value)?;
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }
    };
}

catalog_name!(CatalogScopeName);
catalog_name!(TemplateName);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_slug_like_names() {
        assert!(CatalogScopeName::new("personal-physics").is_ok());
        assert!(TemplateName::new("fancy-unicorn").is_ok());
    }

    #[test]
    fn rejects_empty_and_spaced_names() {
        assert!(matches!(CatalogScopeName::new(""), Err(NameError::Empty)));
        assert!(matches!(
            TemplateName::new("fancy unicorn"),
            Err(NameError::InvalidCharacters { .. })
        ));
    }
}
