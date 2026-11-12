#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! plic: watch, trim and file away video clips.

mod audio;
mod clip_order;
mod clipboard;
mod drafts;
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

use std::path::PathBuf;

fn main() {
    settings::load();
    state::load();
    drafts::load();
    // `plic <folder>` opens that folder; otherwise the working directory, which
    // keeps the old "run it inside the clips folder" workflow working.
    let folder = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .filter(|path| path.is_dir())
        // Without one, carry on where the last run left off.
        .or_else(|| state::get().last_folder().map(std::path::Path::to_path_buf))
        .or_else(|| std::env::current_dir().ok());
    ui::run(folder);
}
