//! What plic remembers between runs: recent folders, window size and
//! position, volume and theme. Kept apart from the user's settings because
//! the app writes it, not the user.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

use serde::{Deserialize, Serialize};

const MAX_RECENT: usize = 8;

/// The window's outer bounds in logical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WindowRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub maximized: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    /// Folders opened before, most recent first.
    pub recent_folders: Vec<PathBuf>,
    pub window: Option<WindowRect>,
    pub volume: f32,
    pub dark_theme: bool,
    /// A release version the user dismissed, so it is not offered again.
    pub dismissed_update: Option<String>,
    /// Unix seconds of the last successful update check.
    pub last_update_check: Option<u64>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            recent_folders: Vec::new(),
            window: None,
            volume: 0.8,
            dark_theme: true,
            dismissed_update: None,
            last_update_check: None,
        }
    }
}

impl State {
    /// The folder to open when none was asked for: the latest one that still
    /// exists.
    pub fn last_folder(&self) -> Option<&Path> {
        self.recent_folders
            .iter()
            .map(PathBuf::as_path)
            .find(|folder| folder.is_dir())
    }

    /// Moves `folder` to the front of the recent folders.
    pub fn remember_folder(&mut self, folder: &Path) {
        self.recent_folders.retain(|known| known != folder);
        self.recent_folders.insert(0, folder.to_path_buf());
        self.recent_folders.truncate(MAX_RECENT);
    }
}

fn current() -> &'static Mutex<State> {
    static CURRENT: OnceLock<Mutex<State>> = OnceLock::new();
    CURRENT.get_or_init(|| Mutex::new(State::default()))
}

fn file() -> Option<PathBuf> {
    crate::settings::config_dir().map(|dir| dir.join("state.json"))
}

/// Reads the saved state, if there is any.
pub fn load() {
    let loaded = file()
        .and_then(|path| fs::read_to_string(path).ok())
        .and_then(|text| serde_json::from_str::<State>(&text).ok());
    if let (Some(state), Ok(mut current)) = (loaded, current().lock()) {
        *current = state;
    }
}

/// A copy of the state as it is now.
pub fn get() -> State {
    current()
        .lock()
        .map(|state| state.clone())
        .unwrap_or_default()
}

/// Changes the state in memory. Call [`save`] to keep it.
pub fn update(change: impl FnOnce(&mut State)) {
    if let Ok(mut state) = current().lock() {
        change(&mut state);
    }
}

/// Writes the state to disk. Failing to is not worth bothering anyone about.
pub fn save() {
    let Some(path) = file() else {
        return;
    };
    let Ok(text) = serde_json::to_string_pretty(&get()) else {
        return;
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).ok();
    }
    fs::write(path, text).ok();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_folders_are_unique_newest_first_and_capped() {
        let mut state = State::default();
        for index in 0..12 {
            state.remember_folder(Path::new(&format!("/clips/{index}")));
        }
        state.remember_folder(Path::new("/clips/5"));
        assert_eq!(state.recent_folders.len(), MAX_RECENT);
        assert_eq!(state.recent_folders[0], Path::new("/clips/5"));
        assert_eq!(
            state
                .recent_folders
                .iter()
                .filter(|folder| **folder == Path::new("/clips/5"))
                .count(),
            1
        );
    }

    #[test]
    fn old_files_missing_fields_still_load() {
        let state: State = serde_json::from_str(r#"{"volume": 0.3}"#).unwrap();
        assert_eq!(state.volume, 0.3);
        assert!(state.dark_theme);
    }
}
