//! Filesystem notifications for the clips folder.

use std::path::Path;

use anyhow::{Context as _, Result};
use futures::channel::mpsc::{UnboundedReceiver, unbounded};
use notify::{RecommendedWatcher, RecursiveMode, Watcher as _};

/// Reports changes directly inside a folder until dropped.
pub struct FolderWatcher {
    _watcher: RecommendedWatcher,
}

impl FolderWatcher {
    /// Starts watching `folder`. Every change sends a unit on the receiver;
    /// the caller is expected to wait for a quiet moment before rescanning.
    pub fn start(folder: &Path) -> Result<(Self, UnboundedReceiver<()>)> {
        let (sender, receiver) = unbounded();
        let mut watcher =
            notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
                if event.is_ok() {
                    sender.unbounded_send(()).ok();
                }
            })
            .context("could not start the folder watcher")?;
        watcher
            .watch(folder, RecursiveMode::NonRecursive)
            .with_context(|| format!("could not watch {}", folder.display()))?;
        Ok((Self { _watcher: watcher }, receiver))
    }
}
