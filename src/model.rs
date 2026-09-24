use std::{
    env::current_dir,
    ffi::OsStr,
    fs::{self, DirEntry},
};

#[derive(Debug)]
pub struct Model {
    pub paths: Vec<DirEntry>,
    pub path_index: usize,
}

impl Model {
    pub fn new() -> Model {
        Model {
            paths: get_current_dir_files(),
            path_index: 0,
        }
    }
}

fn get_current_dir_files() -> Vec<DirEntry> {
    current_dir()
        .map(fs::read_dir)
        .unwrap()
        .unwrap()
        .map(Result::unwrap)
        .filter(|entry| {
            entry
                .path()
                .extension()
                .and_then(OsStr::to_str)
                .is_some_and(|ext| ext == "mp4")
        })
        .collect()
}
