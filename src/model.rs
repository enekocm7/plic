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

const EDITED_DIR: &str = "edited";

#[derive(Debug)]
pub struct ExportResult {
    pub source: PathBuf,
    pub result: std::result::Result<PathBuf, String>,
}

#[derive(Debug)]
pub struct ExportJob {
    pub source: PathBuf,
    pub output: PathBuf,
    pub name: String,
}

#[derive(Debug)]
pub struct Model {
    pub clips: Vec<Clip>,
    pub selected: usize,
    pub folders: Vec<PathBuf>,
    pub editing: bool,
    pub field: EditField,
    pub status: String,
    pub jobs: Vec<ExportJob>,
    pub removed: usize,
    pub pending_delete: Option<PathBuf>,
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
                if entry.file_name() != EDITED_DIR {
                    folders.push(entry.path());
                }
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
            jobs: Vec::new(),
            removed: 0,
            pending_delete: None,
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

    pub fn is_exporting(&self, path: &Path) -> bool {
        self.jobs.iter().any(|job| job.source == path)
    }

    pub fn output_in_use(&self, output: &Path) -> bool {
        self.jobs.iter().any(|job| job.output == output)
    }

    pub fn start_export(
        &mut self,
        source: PathBuf,
        output: PathBuf,
        name: String,
        start: Option<String>,
        duration: Option<String>,
    ) {
        let sender = self.export_tx.clone();
        self.jobs.push(ExportJob {
            source: source.clone(),
            output: output.clone(),
            name: name.clone(),
        });
        self.editing = false;
        self.status = format!("Started exporting {name}");
        if let Some(next) = self.next_pending() {
            self.selected = next;
        }

        std::thread::spawn(move || {
            let result = crate::ffmpeg::export(&source, &output, start.as_deref(), duration.as_deref())
                .map(|()| output)
                .map_err(|error| format!("{error:#}"));
            let _ = sender.send(ExportResult { source, result });
        });
    }

    fn next_pending(&self) -> Option<usize> {
        let count = self.clips.len();
        (1..=count)
            .map(|offset| (self.selected + offset) % count)
            .find(|&index| {
                let clip = &self.clips[index];
                !clip.exported && !self.is_exporting(&clip.path)
            })
    }

    pub fn finish_export(&mut self, result: ExportResult) {
        let name = self
            .jobs
            .iter()
            .position(|job| job.source == result.source)
            .map(|index| self.jobs.remove(index).name);
        match result.result {
            Ok(output) => {
                let archived = archive_original(&result.source);
                if let Some(index) = self.clips.iter().position(|clip| clip.path == result.source) {
                    if archived.is_ok() {
                        self.clips.remove(index);
                        self.removed += 1;
                        if index < self.selected {
                            self.selected -= 1;
                        }
                        self.selected = self.selected.min(self.clips.len().saturating_sub(1));
                    } else {
                        self.clips[index].exported = true;
                    }
                }

                self.status = if self.jobs.is_empty() && self.clips.iter().all(|clip| clip.exported) {
                    "All clips have been exported".to_owned()
                } else {
                    format!("Saved {}", output.display())
                };
                if let Err(error) = archived {
                    self.status = format!("{}; could not move original: {error:#}", self.status);
                }
            }
            Err(error) => {
                let name = name.unwrap_or_default();
                self.status = format!("Export of {name} failed: {error}");
            }
        }
    }

    pub fn delete_current(&mut self) {
        let Some(clip) = self.current() else {
            return;
        };
        let path = clip.path.clone();
        let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();

        if self.is_exporting(&path) {
            self.status = "Cannot delete a clip while it is being exported".to_owned();
            return;
        }
        if self.pending_delete.as_ref() != Some(&path) {
            self.status = format!("Press d again to permanently delete {name} (any other key cancels)");
            self.pending_delete = Some(path);
            return;
        }

        self.pending_delete = None;
        match fs::remove_file(&path) {
            Ok(()) => {
                self.clips.remove(self.selected);
                if self.selected >= self.clips.len() {
                    self.selected = self.clips.len().saturating_sub(1);
                }
                self.status = format!("Deleted {name}");
            }
            Err(error) => self.status = format!("Could not delete {name}: {error}"),
        }
    }

    pub fn folder_for(&self, clip: &Clip) -> Option<&Path> {
        self.folders.get(clip.folder_index).map(PathBuf::as_path)
    }
}

fn archive_original(source: &Path) -> Result<PathBuf> {
    let parent = source.parent().unwrap_or_else(|| Path::new("."));
    let name = source.file_name().context("source has no file name")?;
    let directory = parent.join(EDITED_DIR);
    fs::create_dir_all(&directory)
        .with_context(|| format!("could not create {}", directory.display()))?;
    let destination = directory.join(name);
    if destination.exists() {
        anyhow::bail!("{} already exists", destination.display());
    }
    fs::rename(source, &destination)
        .with_context(|| format!("could not move to {}", destination.display()))?;
    Ok(destination)
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
