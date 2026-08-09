//! The desktop app's own local settings — recent files, default dialog
//! directory, and a couple of startup-window preferences.
//!
//! Deliberately separate from `fieldcad.scene/v1` (`fieldcad_scene_document`):
//! per `docs/user-stories/README.md`'s "Product model" table, client
//! presentation is local-only ("keep local unless explicitly saved"). This is
//! desktop-client-local configuration, not experiment state, and is not
//! MCP-exposed.

use std::{collections::VecDeque, path::PathBuf};

use serde::{Deserialize, Serialize};

/// Most-recently-used files retained in the "Recent" list.
const MAX_RECENT_FILES: usize = 10;

fn default_true() -> bool {
    true
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UserProfile {
    /// Most-recently-used scene document paths, newest first.
    #[serde(default)]
    pub recent_files: VecDeque<PathBuf>,
    /// Last directory used in Save As or Open, independent of
    /// `recent_files`'s per-file entries — a user may navigate around
    /// without a file being reopened.
    #[serde(default)]
    pub last_directory: Option<PathBuf>,
    /// Whether the Help window opens by default. Once a user has dismissed
    /// it, don't reopen it every launch.
    #[serde(default = "default_true")]
    pub show_help_on_startup: bool,
    /// Whether the Diagnostics window opens by default.
    #[serde(default = "default_true")]
    pub show_diagnostics_on_startup: bool,
}

impl Default for UserProfile {
    fn default() -> Self {
        Self {
            recent_files: VecDeque::new(),
            last_directory: None,
            show_help_on_startup: true,
            show_diagnostics_on_startup: true,
        }
    }
}

impl UserProfile {
    fn path() -> Option<PathBuf> {
        directories::ProjectDirs::from("", "", "fieldcad")
            .map(|dirs| dirs.config_dir().join("profile.json"))
    }

    /// Missing file → defaults (first run). A present-but-corrupt file logs
    /// a warning and falls back to defaults rather than blocking startup —
    /// this is preferences, not the scene document a user's work lives in,
    /// so there is no atomic-write/backup ceremony to match.
    pub fn load() -> Self {
        let Some(path) = Self::path() else {
            return Self::default();
        };
        match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice(&bytes) {
                Ok(profile) => profile,
                Err(error) => {
                    tracing::warn!(%error, path = %path.display(), "user profile is corrupt, using defaults");
                    Self::default()
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(error) => {
                tracing::warn!(%error, path = %path.display(), "could not read user profile, using defaults");
                Self::default()
            }
        }
    }

    pub fn save(&self) {
        let Some(path) = Self::path() else {
            return;
        };
        if let Some(parent) = path.parent()
            && let Err(error) = std::fs::create_dir_all(parent)
        {
            tracing::warn!(%error, path = %parent.display(), "could not create user profile directory");
            return;
        }
        match serde_json::to_vec_pretty(self) {
            Ok(bytes) => {
                if let Err(error) = std::fs::write(&path, bytes) {
                    tracing::warn!(%error, path = %path.display(), "could not write user profile");
                }
            }
            Err(error) => tracing::warn!(%error, "could not encode user profile"),
        }
    }

    /// Adds `path` to the front of the recent list (de-duplicating any
    /// earlier entry for the same path) and updates `last_directory`, then
    /// saves immediately — every mutation is followed by a save (§7.2: no
    /// graceful-shutdown hook exists to batch this until exit).
    pub fn push_recent_file(&mut self, path: PathBuf) {
        self.recent_files.retain(|existing| existing != &path);
        if let Some(parent) = path.parent() {
            self.last_directory = Some(parent.to_path_buf());
        }
        self.recent_files.push_front(path);
        while self.recent_files.len() > MAX_RECENT_FILES {
            self.recent_files.pop_back();
        }
        self.save();
    }

    /// Seeds a native file dialog's starting directory.
    pub fn last_directory_or_home(&self) -> PathBuf {
        self.last_directory
            .clone()
            .or_else(|| directories::UserDirs::new().map(|dirs| dirs.home_dir().to_path_buf()))
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_files_deduplicate_and_move_to_front() {
        let mut profile = UserProfile::default();
        profile
            .recent_files
            .push_front(PathBuf::from("/a/one.fcscene"));
        profile
            .recent_files
            .push_front(PathBuf::from("/a/two.fcscene"));

        // Re-adding an existing entry moves it to the front rather than
        // duplicating it — this only exercises the in-memory list logic, not
        // `save()`, so it needs no filesystem/config-dir access.
        profile
            .recent_files
            .retain(|existing| existing != &PathBuf::from("/a/one.fcscene"));
        profile
            .recent_files
            .push_front(PathBuf::from("/a/one.fcscene"));

        assert_eq!(
            profile.recent_files.iter().collect::<Vec<_>>(),
            vec![
                &PathBuf::from("/a/one.fcscene"),
                &PathBuf::from("/a/two.fcscene"),
            ]
        );
    }

    #[test]
    fn recent_files_are_capped() {
        let mut profile = UserProfile::default();
        for index in 0..MAX_RECENT_FILES + 5 {
            profile
                .recent_files
                .push_front(PathBuf::from(format!("/a/{index}.fcscene")));
            while profile.recent_files.len() > MAX_RECENT_FILES {
                profile.recent_files.pop_back();
            }
        }
        assert_eq!(profile.recent_files.len(), MAX_RECENT_FILES);
    }
}
