//! Desktop-owned reusable constant library.
//!
//! This file is deliberately separate from presentation preferences and scene
//! documents. Only the desktop reads it; imports cross into the authoritative
//! runtime as explicit embedded data.

use std::io::Write;
use std::path::{Path, PathBuf};

use fieldcad_expressions::UserConstantLibrary;

/// Errors loading or atomically saving the user constant library.
#[derive(Debug, thiserror::Error)]
pub enum UserConstantLibraryError {
    /// Platform has no discoverable application configuration directory.
    #[error("no user configuration directory is available")]
    NoConfigurationDirectory,
    /// Filesystem operation failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// JSON is invalid or cannot represent the library.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    /// A newer/incompatible library was supplied.
    #[error("unsupported user constant library format '{format}' version {version}")]
    Unsupported { format: String, version: u32 },
}

/// Dedicated library path beside, not inside, `profile.json`.
pub fn path() -> Result<PathBuf, UserConstantLibraryError> {
    directories::ProjectDirs::from("", "", "fieldcad")
        .map(|dirs| dirs.config_dir().join("user-constants.json"))
        .ok_or(UserConstantLibraryError::NoConfigurationDirectory)
}

/// Load a library, returning an empty one on first use.
pub fn load_from(path: &Path) -> Result<UserConstantLibrary, UserConstantLibraryError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(UserConstantLibrary::default());
        }
        Err(error) => return Err(error.into()),
    };
    let library: UserConstantLibrary = serde_json::from_slice(&bytes)?;
    if library.format != "fieldcad.user-constants/v1" || library.format_version > 1 {
        return Err(UserConstantLibraryError::Unsupported {
            format: library.format,
            version: library.format_version,
        });
    }
    Ok(library)
}

/// Atomically replace a library using a same-directory temporary file.
pub fn save_to(path: &Path, library: &UserConstantLibrary) -> Result<(), UserConstantLibraryError> {
    let parent = path
        .parent()
        .ok_or(UserConstantLibraryError::NoConfigurationDirectory)?;
    std::fs::create_dir_all(parent)?;
    let bytes = serde_json::to_vec_pretty(library)?;
    let temporary = path.with_extension("json.tmp");
    let mut file = std::fs::File::create(&temporary)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(temporary, path)?;
    Ok(())
}

/// Load from the platform path.
pub fn load() -> Result<UserConstantLibrary, UserConstantLibraryError> {
    load_from(&path()?)
}

/// Save to the platform path.
pub fn save(library: &UserConstantLibrary) -> Result<(), UserConstantLibraryError> {
    save_to(&path()?, library)
}

/// Open the folder containing `user-constants.json` in the OS file manager.
///
/// Opens the parent directory rather than the file itself, since handing a
/// `.json` path to `open::that` launches a text editor, not a file manager.
pub fn reveal_containing_folder() -> Result<(), UserConstantLibraryError> {
    let path = path()?;
    let parent = path
        .parent()
        .ok_or(UserConstantLibraryError::NoConfigurationDirectory)?;
    std::fs::create_dir_all(parent)?;
    open::that(parent).map_err(UserConstantLibraryError::Io)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn library_file_round_trips_and_first_use_is_empty() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("user-constants.json");
        assert_eq!(load_from(&path).unwrap(), UserConstantLibrary::default());
        let library = UserConstantLibrary::default();
        save_to(&path, &library).unwrap();
        assert_eq!(load_from(&path).unwrap(), library);
        assert!(!path.with_extension("json.tmp").exists());
    }
}
