#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! plic: watch, trim and file away video clips.

mod audio;
mod clipboard;
mod ffmpeg;
mod library;
mod playback;
mod scheduler;
mod settings;
mod timestamp;
mod trim;
mod ui;
mod worker;

use std::path::PathBuf;

fn main() {
    settings::load();
    // `plic <folder>` opens that folder; otherwise the working directory, which
    // keeps the old "run it inside the clips folder" workflow working.
    let folder = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .filter(|path| path.is_dir())
        .or_else(|| std::env::current_dir().ok());
    ui::run(folder);
}
