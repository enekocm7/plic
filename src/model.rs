use std::{
    env::current_dir,
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
        .collect()
}
