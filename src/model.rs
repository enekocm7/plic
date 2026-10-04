use std::{
    env::current_dir,
    fs,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, Sender},
};

use anyhow::{Context, Result};

use crate::text_input::TextInput;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditField {
    Name,
    Start,
    End,
    Folder,
}

impl EditField {
    pub fn next(self) -> Self {
        match self {
            Self::Name => Self::Start,
            Self::Start => Self::End,
            Self::End => Self::Folder,
            Self::Folder => Self::Name,
        }
    }

    pub fn previous(self) -> Self {
        match self {
            Self::Name => Self::Folder,
            Self::Start => Self::Name,
            Self::End => Self::Start,
            Self::Folder => Self::End,
        }
    }
}

#[derive(Debug)]
pub struct Clip {
    pub path: PathBuf,
    pub name: TextInput,
    pub start: TextInput,
    pub end: TextInput,
    pub folder_index: usize,
    pub exported: bool,
}

impl Clip {
    fn new(path: PathBuf) -> Self {
        let name = path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default();
        Self {
            path,
            name: TextInput::from(&name),
            start: TextInput::from("0:00"),
            end: TextInput::new(),
            folder_index: 0,
            exported: false,
        }
    }
}

#[derive(Debug)]
pub struct ExportResult {
    pub source: PathBuf,
    pub result: std::result::Result<PathBuf, String>,
}

#[derive(Debug)]
pub struct Model {
    pub clips: Vec<Clip>,
    pub selected: usize,
    pub folders: Vec<PathBuf>,
    pub editing: bool,
    pub field: EditField,
    pub status: String,
    pub exporting: Option<PathBuf>,
    export_tx: Sender<ExportResult>,
    export_rx: Receiver<ExportResult>,
}

impl Model {
    pub fn new() -> Result<Self> {
        let directory = current_dir().context("could not read the current directory")?;
        let entries = fs::read_dir(&directory)
            .with_context(|| format!("could not list {}", directory.display()))?;
        let mut clips = Vec::new();
        let mut folders = Vec::new();

        for entry in entries {
            let entry = entry.context("could not read a directory entry")?;
            let kind = entry
                .file_type()
                .with_context(|| format!("could not inspect {}", entry.path().display()))?;
            if kind.is_dir() {
                folders.push(entry.path());
            } else if kind.is_file()
                && entry
                    .path()
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("mp4"))
            {
                clips.push(Clip::new(entry.path()));
            }
        }

        clips.sort_by_key(|clip| {
            clip.path
                .file_name()
                .map(|name| name.to_string_lossy().to_lowercase())
        });
        folders.sort_by_key(|path| {
            path.file_name()
                .map(|name| name.to_string_lossy().to_lowercase())
        });

        let (export_tx, export_rx) = mpsc::channel();
        let status = if clips.is_empty() {
            format!("No .mp4 files found in {}", directory.display())
        } else {
            format!("Found {} clips in {}", clips.len(), directory.display())
        };

        Ok(Self {
            clips,
            selected: 0,
            folders,
            editing: false,
            field: EditField::Name,
            status,
            exporting: None,
            export_tx,
            export_rx,
        })
    }

    pub fn current(&self) -> Option<&Clip> {
        self.clips.get(self.selected)
    }

    pub fn current_mut(&mut self) -> Option<&mut Clip> {
        self.clips.get_mut(self.selected)
    }

    pub fn try_export_result(&self) -> Option<ExportResult> {
        self.export_rx.try_recv().ok()
    }

    pub fn start_export(
        &mut self,
        source: PathBuf,
        output: PathBuf,
        start: Option<String>,
        duration: Option<String>,
    ) {
        let sender = self.export_tx.clone();
        self.exporting = Some(source.clone());
        self.editing = false;
        self.status = format!("Exporting {}…", source.file_name().unwrap_or_default().to_string_lossy());

        std::thread::spawn(move || {
            let result = crate::ffmpeg::export(&source, &output, start.as_deref(), duration.as_deref())
                .map(|()| output)
                .map_err(|error| format!("{error:#}"));
            let _ = sender.send(ExportResult { source, result });
        });
    }

    pub fn finish_export(&mut self, result: ExportResult) {
        self.exporting = None;
        match result.result {
            Ok(output) => {
                if let Some(clip) = self.clips.iter_mut().find(|clip| clip.path == result.source) {
                    clip.exported = true;
                }

                let was_on_exported_clip = self
                    .current()
                    .is_some_and(|clip| clip.path == result.source);
                if was_on_exported_clip {
                    if let Some(next) = next_unexported(&self.clips, self.selected) {
                        self.selected = next;
                        self.status = format!(
                            "Saved {}; moved to the next unprocessed clip",
                            output.file_name().unwrap_or_default().to_string_lossy()
                        );
                    } else {
                        self.status = "All clips have been exported".to_owned();
                    }
                } else {
                    self.status = format!("Saved {}", output.display());
                }
            }
            Err(error) => {
                self.status = format!("Export failed: {error}");
            }
        }
    }

    pub fn folder_for(&self, clip: &Clip) -> Option<&Path> {
        self.folders.get(clip.folder_index).map(PathBuf::as_path)
    }
}

fn next_unexported(clips: &[Clip], selected: usize) -> Option<usize> {
    if clips.is_empty() {
        return None;
    }
    (1..=clips.len())
        .map(|offset| (selected + offset) % clips.len())
        .find(|&index| !clips[index].exported)
}

pub fn parse_timestamp(value: &str) -> Option<u64> {
    let parts: Vec<_> = value.trim().split(':').collect();
    if !(2..=3).contains(&parts.len()) {
        return None;
    }

    let numbers: Option<Vec<u64>> = parts.iter().map(|part| part.parse().ok()).collect();
    let numbers = numbers?;
    let (hours, minutes, seconds) = if numbers.len() == 3 {
        if numbers[1] >= 60 || numbers[2] >= 60 {
            return None;
        }
        (numbers[0], numbers[1], numbers[2])
    } else {
        if numbers[1] >= 60 {
            return None;
        }
        (0, numbers[0], numbers[1])
    };
    hours
        .checked_mul(3600)?
        .checked_add(minutes.checked_mul(60)?)?
        .checked_add(seconds)
}

pub fn format_timestamp(seconds: u64) -> String {
    let hours = seconds / 3600;
    let minutes = (seconds % 3600) / 60;
    let seconds = seconds % 60;
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}
