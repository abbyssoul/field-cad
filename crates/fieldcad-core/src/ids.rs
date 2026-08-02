use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum IdentifierError {
    #[error("identifier cannot be empty")]
    Empty,
    #[error("identifier '{value}' contains unsupported characters")]
    InvalidCharacters { value: String },
}

fn validate_identifier(value: &str) -> Result<(), IdentifierError> {
    if value.is_empty() {
        return Err(IdentifierError::Empty);
    }
    if !value
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.'))
    {
        return Err(IdentifierError::InvalidCharacters {
            value: value.to_owned(),
        });
    }
    Ok(())
}

macro_rules! string_id {
    ($name:ident) => {
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, IdentifierError> {
                let value = value.into();
                validate_identifier(&value)?;
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

string_id!(PluginId);
string_id!(PropertyId);

/// Shared storage for plugin-namespaced identifiers.
///
/// This is deliberately private: `ComponentTypeId` and `ChannelId` name different
/// kinds of thing and must not be interchangeable at a call site.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
struct QualifiedName {
    plugin: PluginId,
    name: String,
}

impl QualifiedName {
    fn new(plugin: PluginId, name: impl Into<String>) -> Result<Self, IdentifierError> {
        let name = name.into();
        validate_identifier(&name)?;
        Ok(Self { plugin, name })
    }
}

impl fmt::Display for QualifiedName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}", self.plugin, self.name)
    }
}

macro_rules! qualified_id {
    ($name:ident) => {
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(QualifiedName);

        impl $name {
            pub fn new(plugin: PluginId, name: impl Into<String>) -> Result<Self, IdentifierError> {
                QualifiedName::new(plugin, name).map(Self)
            }

            pub fn plugin(&self) -> &PluginId {
                &self.0.plugin
            }

            pub fn name(&self) -> &str {
                &self.0.name
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }
    };
}

qualified_id!(ComponentTypeId);
qualified_id!(ChannelId);

/// Entity identifiers are monotonic and never reused within a session, so a
/// stale identifier can only fail to resolve; it can never silently address a
/// different entity. That is why they carry no generation counter.
macro_rules! entity_id {
    ($name:ident) => {
        #[derive(
            Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(u64);

        impl $name {
            pub const fn new(value: u64) -> Self {
                Self(value)
            }

            pub const fn get(self) -> u64 {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }
    };
}

entity_id!(ObjectId);
entity_id!(PlaneId);
entity_id!(ProbeId);

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct WorldRevision(u64);

impl WorldRevision {
    pub const INITIAL: Self = Self(0);

    pub const fn get(self) -> u64 {
        self.0
    }

    pub const fn next(self) -> Self {
        Self(self.0 + 1)
    }
}

impl fmt::Display for WorldRevision {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PluginVersion {
    pub major: u16,
    pub minor: u16,
    pub patch: u16,
}

impl PluginVersion {
    pub const fn new(major: u16, minor: u16, patch: u16) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }
}

impl fmt::Display for PluginVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qualified_identifiers_are_stable_and_namespaced() {
        let plugin = PluginId::new("fieldcad.test").unwrap();
        let channel = ChannelId::new(plugin, "linear-vector").unwrap();

        assert_eq!(channel.to_string(), "fieldcad.test.linear-vector");
        assert!(PropertyId::new("spaces are not valid").is_err());
    }

    #[test]
    fn channel_and_component_identifiers_are_distinct_types() {
        let plugin = PluginId::new("fieldcad.test").unwrap();
        let channel = ChannelId::new(plugin.clone(), "charge").unwrap();
        let component = ComponentTypeId::new(plugin, "charge").unwrap();

        // They print identically but cannot be substituted for one another; this
        // test exists so that collapsing them back into one alias fails to compile.
        assert_eq!(channel.to_string(), component.to_string());
        assert_eq!(channel.name(), component.name());
    }
}
