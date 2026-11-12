#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! plic: watch, trim and file away video clips.

mod audio;
mod clip_order;
mod clipboard;
mod ffmpeg;
mod library;
mod markers;
mod naming;
mod playback;
mod scheduler;
mod settings;
mod state;
mod timestamp;
mod trim;
mod ui;
mod updates;
mod watcher;
mod worker;

use std::path::{Path, PathBuf};

fn main() {
    settings::load();
    state::load();
    let argument = std::env::args_os().nth(1).map(PathBuf::from);
    let (folder, clip) = launch_target(argument.as_deref());
    let folder = folder
        // Without one, carry on where the last run left off.
        .or_else(|| state::get().last_folder().map(Path::to_path_buf))
        // Otherwise the working directory, which keeps the old "run it
        // inside the clips folder" workflow working.
        .or_else(|| std::env::current_dir().ok());
    ui::run(folder, clip);
}

/// What `plic <path>` opens: a folder, or the folder of a video with that
/// clip selected, as Explorer's "Open with plic" passes either.
fn launch_target(path: Option<&Path>) -> (Option<PathBuf>, Option<PathBuf>) {
    match path {
        Some(path) if path.is_dir() => (Some(path.to_path_buf()), None),
        Some(path) if path.is_file() && library::is_video(path) => {
            let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
            (path.parent().map(Path::to_path_buf), Some(path))
        }
        _ => (None, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opens_a_folder_or_the_folder_of_a_video() {
        let root = std::env::temp_dir().join(format!("plic-launch-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let clip = root.join("clip.mp4");
        let notes = root.join("notes.txt");
        std::fs::write(&clip, b"").unwrap();
        std::fs::write(&notes, b"").unwrap();

        assert_eq!(launch_target(Some(&root)), (Some(root.clone()), None));
        assert_eq!(
            launch_target(Some(&clip)),
            (Some(root.clone()), Some(clip.clone()))
        );
        assert_eq!(launch_target(Some(&notes)), (None, None));
        assert_eq!(launch_target(None), (None, None));
        std::fs::remove_dir_all(&root).unwrap();
    }
}
