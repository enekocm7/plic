//! User settings, stored as JSON in the app config directory.

use std::{
    collections::BTreeMap,
    fs,
    path::PathBuf,
    sync::{OnceLock, RwLock},
};

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use crate::ffmpeg::Quality;

/// The folder originals are moved to after export, unless changed.
pub const DEFAULT_ARCHIVE_FOLDER: &str = "edited";
pub const DEFAULT_SEEK_STEP_SECS: u32 = 5;
const MAX_SEEK_STEP_SECS: u32 = 600;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// How far the arrow keys move the playhead.
    pub seek_step_secs: u32,
    /// Folder, inside the clips folder, that exported originals move to.
    pub archive_folder: String,
    /// Name of the subfolder, or "Clips folder" for the clips folder itself,
    /// that exports go to when nothing else was chosen. Empty uses the one
    /// used last.
    pub default_destination: String,
    /// Template for export names, such as `{folder}-{date}-{n}`. Empty
    /// keeps the clip's own name. `{n}` counts up past names already used.
    pub name_template: String,
    /// Folder holding `ffmpeg` and `ffprobe` to use instead of plic's own
    /// copy. Empty uses plic's copy.
    pub ffmpeg_folder: String,
    /// How hard re-encoded exports are compressed.
    pub export_quality: Quality,
    /// Keys for commands, by command name, replacing the default keys.
    /// Commands left out keep their defaults; an empty list unbinds one.
    pub key_bindings: BTreeMap<String, Vec<String>>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            seek_step_secs: DEFAULT_SEEK_STEP_SECS,
            archive_folder: DEFAULT_ARCHIVE_FOLDER.to_owned(),
            default_destination: String::new(),
            name_template: String::new(),
            ffmpeg_folder: String::new(),
            export_quality: Quality::default(),
            key_bindings: BTreeMap::new(),
        }
    }
}

impl Settings {
    /// Replaces values that cannot be used with their defaults.
    pub fn sanitized(mut self) -> Self {
        self.seek_step_secs = self.seek_step_secs.clamp(1, MAX_SEEK_STEP_SECS);
        self.archive_folder = self.archive_folder.trim().to_owned();
        if crate::library::validate_file_stem(&self.archive_folder).is_err() {
            self.archive_folder = DEFAULT_ARCHIVE_FOLDER.to_owned();
        }
        self.default_destination = self.default_destination.trim().to_owned();
        self.name_template = self.name_template.trim().to_owned();
        self.ffmpeg_folder = self.ffmpeg_folder.trim().to_owned();
        self
    }
}

fn current() -> &'static RwLock<Settings> {
    static CURRENT: OnceLock<RwLock<Settings>> = OnceLock::new();
    CURRENT.get_or_init(|| RwLock::new(Settings::default()))
}

/// The settings in effect.
pub fn get() -> Settings {
    current()
        .read()
        .map(|settings| settings.clone())
        .unwrap_or_default()
}

/// The folder plic keeps its files in.
pub fn config_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|dir| dir.join("plic"))
}

fn file() -> Option<PathBuf> {
    config_dir().map(|dir| dir.join("settings.json"))
}

/// Reads the settings file, if there is one. A missing or unreadable file
/// leaves the defaults.
pub fn load() {
    let Some(text) = file().and_then(|path| fs::read_to_string(path).ok()) else {
        return;
    };
    if let Ok(settings) = serde_json::from_str::<Settings>(&text)
        && let Ok(mut current) = current().write()
    {
        *current = settings.sanitized();
    }
}

/// Applies new settings and saves them.
pub fn update(settings: Settings) -> Result<()> {
    let settings = settings.sanitized();
    if let Ok(mut current) = current().write() {
        *current = settings.clone();
    }
    let path = file().context("there is no config directory")?;
    fs::create_dir_all(path.parent().context("settings path has no parent")?)
        .context("could not create the config directory")?;
    fs::write(&path, serde_json::to_string_pretty(&settings)?)
        .with_context(|| format!("could not write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_fields_take_defaults() {
        let settings: Settings = serde_json::from_str(r#"{"seek_step_secs": 10}"#).unwrap();
        assert_eq!(settings.seek_step_secs, 10);
        assert_eq!(settings.archive_folder, DEFAULT_ARCHIVE_FOLDER);
        assert_eq!(settings.export_quality, Quality::Balanced);
    }

    #[test]
    fn unusable_values_are_replaced() {
        let settings = Settings {
            seek_step_secs: 0,
            archive_folder: "a/b".into(),
            default_destination: "  Best  ".into(),
            name_template: String::new(),
            ffmpeg_folder: String::new(),
            export_quality: Quality::default(),
            key_bindings: BTreeMap::new(),
        }
        .sanitized();
        assert_eq!(settings.seek_step_secs, 1);
        assert_eq!(settings.archive_folder, DEFAULT_ARCHIVE_FOLDER);
        assert_eq!(settings.default_destination, "Best");
    }
}
