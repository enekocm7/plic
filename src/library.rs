//! The clips folder: which videos it holds, which subfolders exports can go
//! to, and the file operations performed on originals.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::{Duration, SystemTime},
};

use anyhow::{Context, Result, bail};

/// Originals are moved here once exported, so they drop out of the queue.
/// Name of the folder exported originals move to.
pub fn edited_dir() -> String {
    crate::settings::get().archive_folder
}

const VIDEO_EXTENSIONS: [&str; 5] = ["mp4", "mov", "mkv", "webm", "m4v"];

/// A video file found in the clips folder.
#[derive(Debug, Clone)]
pub struct ClipFile {
    pub path: PathBuf,
    pub size: u64,
    pub modified: Option<SystemTime>,
}

impl ClipFile {
    pub fn file_name(&self) -> String {
        display_name(&self.path)
    }

    pub fn stem(&self) -> String {
        self.path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default()
    }
}

/// The contents of a clips folder.
#[derive(Debug, Clone)]
pub struct Library {
    pub clips: Vec<ClipFile>,
    /// Originals already exported and moved to [`edited_dir`], newest first.
    pub settled: Vec<ClipFile>,
    /// Where exports can be saved: the clips folder itself first, then its
    /// subfolders sorted by name.
    pub folders: Vec<PathBuf>,
}

impl Library {
    /// Lists the videos and destination folders directly inside `root`.
    pub fn scan(root: &Path) -> Result<Self> {
        let entries =
            fs::read_dir(root).with_context(|| format!("could not list {}", root.display()))?;
        let mut clips = Vec::new();
        let mut folders = Vec::new();
        // Exports saved into the clips folder are results, not recordings.
        let exported = crate::state::get().root_exports;

        for entry in entries {
            let entry = entry.context("could not read a directory entry")?;
            let path = entry.path();
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if metadata.is_dir() {
                let hidden = entry.file_name().to_string_lossy().starts_with('.');
                if entry.file_name().to_string_lossy() != edited_dir() && !hidden {
                    folders.push(path);
                }
            } else if !exported.contains(&path)
                && let Some(clip) = ClipFile::from_entry(path, &metadata)
            {
                clips.push(clip);
            }
        }
        clips.sort_by_cached_key(|clip| sort_key(&clip.path));
        folders.sort_by_cached_key(|path| sort_key(path));
        folders.insert(0, root.to_path_buf());

        // A missing or unreadable edited folder just means nothing is settled.
        let mut settled: Vec<ClipFile> = fs::read_dir(root.join(edited_dir()))
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| {
                let metadata = entry.metadata().ok()?;
                ClipFile::from_entry(entry.path(), &metadata)
            })
            .collect();
        settled.sort_by_key(|clip| std::cmp::Reverse(clip.modified));

        Ok(Self {
            clips,
            settled,
            folders,
        })
    }
}

impl ClipFile {
    fn from_entry(path: PathBuf, metadata: &fs::Metadata) -> Option<Self> {
        (metadata.is_file() && is_video(&path) && !is_partial_export(&path)).then(|| Self {
            path,
            size: metadata.len(),
            modified: metadata.modified().ok(),
        })
    }
}

fn sort_key(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

pub fn is_video(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            VIDEO_EXTENSIONS
                .iter()
                .any(|known| extension.eq_ignore_ascii_case(known))
        })
}

/// Temporary files written by an export that is still running.
fn is_partial_export(path: &Path) -> bool {
    path.file_name()
        .map(|name| name.to_string_lossy())
        .is_some_and(|name| name.starts_with('.') && name.contains(".plic-"))
}

/// Moves an exported original into the `edited` folder beside it.
pub fn archive_original(source: &Path) -> Result<PathBuf> {
    let parent = source.parent().unwrap_or_else(|| Path::new("."));
    let name = source.file_name().context("source has no file name")?;
    let directory = parent.join(edited_dir());
    fs::create_dir_all(&directory)
        .with_context(|| format!("could not create {}", directory.display()))?;
    let destination = directory.join(name);
    if destination.exists() {
        bail!("{} already exists", destination.display());
    }
    fs::rename(source, &destination)
        .with_context(|| format!("could not move to {}", destination.display()))?;
    Ok(destination)
}

/// Moves an archived original from [`edited_dir`] back into the clips
/// folder, undoing [`archive_original`].
pub fn restore_original(archived: &Path) -> Result<PathBuf> {
    let name = archived.file_name().context("clip has no file name")?;
    let root = archived
        .parent()
        .and_then(Path::parent)
        .context("clip is not inside a clips folder")?;
    let destination = root.join(name);
    if destination.exists() {
        bail!("{} already exists", destination.display());
    }
    fs::rename(archived, &destination)
        .with_context(|| format!("could not move to {}", destination.display()))?;
    Ok(destination)
}

/// Moves a clip into `folder`, keeping its name. Fails rather than
/// overwrite a file of the same name.
pub fn move_into(source: &Path, folder: &Path) -> Result<PathBuf> {
    let name = source.file_name().context("clip has no file name")?;
    let destination = folder.join(name);
    if destination.exists() {
        bail!("{} already exists", destination.display());
    }
    fs::rename(source, &destination)
        .or_else(|_| {
            // Another drive: copy, then remove the original.
            fs::copy(source, &destination)?;
            fs::remove_file(source)
        })
        .with_context(|| {
            format!(
                "could not move {} to {}",
                display_name(source),
                folder.display()
            )
        })?;
    Ok(destination)
}

/// Sends a clip to the recycle bin / trash so a mistaken delete can be undone.
pub fn trash_clip(path: &Path) -> Result<()> {
    trash::delete(path)
        .with_context(|| format!("could not move {} to the trash", display_name(path)))
}

/// Takes the most recently deleted file that was at `path` back out of the
/// recycle bin / trash.
#[cfg(any(
    target_os = "windows",
    all(
        unix,
        not(target_os = "macos"),
        not(target_os = "ios"),
        not(target_os = "android")
    )
))]
pub fn restore_from_trash(path: &Path) -> Result<()> {
    let item = trash::os_limited::list()
        .context("could not read the recycle bin")?
        .into_iter()
        .filter(|item| item.original_path() == path)
        .max_by_key(|item| item.time_deleted)
        .with_context(|| format!("{} is no longer in the recycle bin", display_name(path)))?;
    if path.exists() {
        bail!("{} already exists", path.display());
    }
    trash::os_limited::restore_all([item])
        .with_context(|| format!("could not restore {}", display_name(path)))
}

#[cfg(not(any(
    target_os = "windows",
    all(
        unix,
        not(target_os = "macos"),
        not(target_os = "ios"),
        not(target_os = "android")
    )
)))]
pub fn restore_from_trash(path: &Path) -> Result<()> {
    bail!(
        "restore {} from the Trash in Finder; plic cannot do it on this system",
        display_name(path)
    )
}

/// Runs a file operation, retrying for a moment while it fails. On Windows a
/// file cannot be moved or deleted while a just-stopped decoder still has it
/// open.
pub fn retry_while_busy<T>(mut operation: impl FnMut() -> Result<T>) -> Result<T> {
    const ATTEMPTS: u32 = 10;
    const DELAY: Duration = Duration::from_millis(200);
    let mut attempt = 1;
    loop {
        match operation() {
            Err(_) if attempt < ATTEMPTS => {
                attempt += 1;
                thread::sleep(DELAY);
            }
            result => return result,
        }
    }
}

/// Opens the system file manager with `path` selected where supported.
pub fn show_in_file_manager(path: &Path) -> Result<()> {
    let mut command = if cfg!(windows) {
        // Explorer wants `/select,` and the path as one argument.
        let mut select = std::ffi::OsString::from("/select,");
        select.push(path);
        let mut command = Command::new("explorer");
        command.arg(select);
        command
    } else if cfg!(target_os = "macos") {
        let mut command = Command::new("open");
        command.arg("-R").arg(path);
        command
    } else {
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        return open::that_detached(parent)
            .with_context(|| format!("could not open {}", parent.display()));
    };
    // Explorer reports a failure exit code even when it works, so only a
    // failure to start counts.
    command
        .spawn()
        .map(drop)
        .context("could not start the file manager")
}

/// Why a name cannot be used for an exported file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameError {
    Empty,
    ReservedCharacter,
    TrailingDotOrSpace,
    ReservedDeviceName,
}

impl std::fmt::Display for NameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Empty => "Enter a name for the exported clip",
            Self::ReservedCharacter => {
                "Names cannot contain < > : \" / \\ | ? * or control characters"
            }
            Self::TrailingDotOrSpace => "Names cannot end with a dot or a space",
            Self::ReservedDeviceName => "That name is reserved by Windows",
        })
    }
}

impl std::error::Error for NameError {}

/// Checks that `name` is usable as a file stem on every platform we support.
pub fn validate_file_stem(name: &str) -> Result<(), NameError> {
    if name.is_empty() || name == "." || name == ".." {
        return Err(NameError::Empty);
    }
    if name
        .chars()
        .any(|character| character.is_control() || "<>:\"/\\|?*".contains(character))
    {
        return Err(NameError::ReservedCharacter);
    }
    if name.ends_with([' ', '.']) {
        return Err(NameError::TrailingDotOrSpace);
    }

    let device = name.split('.').next().unwrap_or(name).to_ascii_uppercase();
    let reserved = matches!(device.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ["COM", "LPT"].iter().any(|prefix| {
            device.strip_prefix(prefix).is_some_and(|number| {
                number.len() == 1 && matches!(number.as_bytes()[0], b'1'..=b'9')
            })
        });
    if reserved {
        return Err(NameError::ReservedDeviceName);
    }
    Ok(())
}

pub fn display_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archives_and_restores_originals() -> Result<()> {
        let root = std::env::temp_dir().join(format!("plic-library-{}", std::process::id()));
        fs::create_dir_all(&root)?;
        let clip = root.join("clip.mp4");
        fs::write(&clip, b"video")?;

        let archived = archive_original(&clip)?;
        assert_eq!(archived, root.join(edited_dir()).join("clip.mp4"));
        let library = Library::scan(&root)?;
        assert!(library.clips.is_empty());
        assert_eq!(library.settled.len(), 1);

        assert_eq!(restore_original(&archived)?, clip);
        let library = Library::scan(&root)?;
        assert_eq!(library.clips.len(), 1);
        assert!(library.settled.is_empty());

        fs::remove_dir_all(&root)?;
        Ok(())
    }

    /// Uses the real recycle bin, so it only runs when asked for.
    #[test]
    #[ignore]
    fn restores_trashed_clips() -> Result<()> {
        let root = std::env::temp_dir().join(format!("plic-trash-{}", std::process::id()));
        fs::create_dir_all(&root)?;
        let clip = root.join("trashed.mp4");
        fs::write(&clip, b"video")?;
        trash_clip(&clip)?;
        assert!(!clip.exists());
        restore_from_trash(&clip)?;
        assert_eq!(fs::read(&clip)?, b"video");
        fs::remove_dir_all(&root)?;
        Ok(())
    }

    #[test]
    fn accepts_ordinary_names() {
        for name in ["clip", "Best play 2", "a.b", "COM10", "console"] {
            assert_eq!(validate_file_stem(name), Ok(()), "{name:?}");
        }
    }

    #[test]
    fn rejects_unsafe_names() {
        assert_eq!(validate_file_stem(""), Err(NameError::Empty));
        assert_eq!(validate_file_stem(".."), Err(NameError::Empty));
        assert_eq!(validate_file_stem("a/b"), Err(NameError::ReservedCharacter));
        assert_eq!(
            validate_file_stem("what?"),
            Err(NameError::ReservedCharacter)
        );
        assert_eq!(
            validate_file_stem("name."),
            Err(NameError::TrailingDotOrSpace)
        );
        assert_eq!(
            validate_file_stem("name "),
            Err(NameError::TrailingDotOrSpace)
        );
        assert_eq!(
            validate_file_stem("con"),
            Err(NameError::ReservedDeviceName)
        );
        assert_eq!(
            validate_file_stem("LPT3.final"),
            Err(NameError::ReservedDeviceName)
        );
    }

    #[test]
    fn scan_lists_videos_and_destination_folders() -> Result<()> {
        let root = std::env::temp_dir().join(format!("plic-scan-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("Highlights"))?;
        fs::create_dir_all(root.join(edited_dir()))?;
        fs::create_dir_all(root.join(".cache"))?;
        for file in ["b.MP4", "a.mkv", "notes.txt", ".a.plic-1-0.mp4"] {
            fs::write(root.join(file), b"")?;
        }

        let library = Library::scan(&root)?;
        let clips: Vec<_> = library.clips.iter().map(ClipFile::file_name).collect();
        let folders: Vec<_> = library
            .folders
            .iter()
            .map(|path| display_name(path))
            .collect();
        fs::remove_dir_all(&root)?;

        assert_eq!(clips, ["a.mkv", "b.MP4"]);
        assert_eq!(folders[1..], ["Highlights"]);
        assert_eq!(library.folders[0], root);
        Ok(())
    }
}
