//! Undo and redo for changes to clips: trims, deletes and moving originals
//! into the archive folder after export.

use std::path::{Path, PathBuf};

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::notification::Notification;
use gpui_kit::*;

use super::app::PlicApp;
use crate::{library, trim::Segments};

/// Changes kept for undoing.
const LIMIT: usize = 100;

/// A change that can be undone and redone.
#[derive(Debug, Clone, PartialEq)]
pub enum Change {
    /// A clip's segments changed from `before` to `after`.
    Trim {
        path: PathBuf,
        before: Segments,
        after: Segments,
    },
    /// A clip went to the recycle bin.
    Delete { path: PathBuf },
    /// An exported original moved into the archive folder.
    Archive {
        original: PathBuf,
        archived: PathBuf,
    },
}

impl Change {
    /// What undoing or redoing this does, for tooltips and notifications.
    pub fn describe(&self) -> String {
        let name = |path: &Path| library::display_name(path);
        match self {
            Self::Trim { path, .. } => format!("trim of {}", name(path)),
            Self::Delete { path } => format!("delete of {}", name(path)),
            Self::Archive { original, .. } => {
                format!("move of {} to {}", name(original), library::edited_dir())
            }
        }
    }
}

/// The undo and redo stacks.
#[derive(Default)]
pub struct History {
    undo: Vec<Change>,
    redo: Vec<Change>,
    /// The trim being watched for changes: the selected clip's path and its
    /// segments as last recorded.
    watched: Option<(PathBuf, Segments)>,
    /// Whether the last trim change may still grow, e.g. during a drag.
    open_trim: bool,
    /// A file operation for undo or redo is running; more presses wait.
    pub busy: bool,
}

/// Whether two sets of segments differ in a way worth undoing. Picking
/// another segment to edit is not.
fn same_trim(a: &Segments, b: &Segments) -> bool {
    a.ranges() == b.ranges() && a.named_parts("") == b.named_parts("")
}

impl History {
    /// Records a new change. Anything undone before can no longer be
    /// redone.
    pub fn record(&mut self, change: Change) {
        self.redo.clear();
        self.push_undo(change);
    }

    fn push_undo(&mut self, change: Change) {
        self.open_trim = false;
        self.undo.push(change);
        if self.undo.len() > LIMIT {
            self.undo.remove(0);
        }
    }

    pub fn next_undo(&self) -> Option<&Change> {
        self.undo.last()
    }

    pub fn next_redo(&self) -> Option<&Change> {
        self.redo.last()
    }

    /// Notes the segments of the selected clip, recording a trim change if
    /// they changed since last time. While `merging`, as during a drag,
    /// consecutive changes become one.
    pub fn watch_trim(&mut self, path: &Path, segments: &Segments, merging: bool) {
        let Some((watched, before)) = &self.watched else {
            self.watched = Some((path.to_path_buf(), segments.clone()));
            return;
        };
        if watched != path {
            self.open_trim = false;
            self.watched = Some((path.to_path_buf(), segments.clone()));
            return;
        }
        if same_trim(before, segments) {
            if !merging {
                self.open_trim = false;
            }
            return;
        }
        let before = before.clone();
        self.watched = Some((path.to_path_buf(), segments.clone()));
        if self.open_trim
            && let Some(Change::Trim {
                path: last, after, ..
            }) = self.undo.last_mut()
            && last == path
        {
            *after = segments.clone();
        } else {
            self.record(Change::Trim {
                path: path.to_path_buf(),
                before,
                after: segments.clone(),
            });
        }
        self.open_trim = merging;
    }

    /// Stops watching, e.g. when no clip is selected.
    pub fn unwatch(&mut self) {
        self.watched = None;
        self.open_trim = false;
    }

    /// Makes `segments` the recorded state of `path`, so setting them is
    /// not taken for a new change.
    fn set_watched(&mut self, path: &Path, segments: &Segments) {
        self.watched = Some((path.to_path_buf(), segments.clone()));
        self.open_trim = false;
    }

    /// Follows a clip to a new path, e.g. after it was archived or restored.
    fn rename(&mut self, from: &Path, to: &Path) {
        for change in self.undo.iter_mut().chain(&mut self.redo) {
            match change {
                Change::Trim { path, .. } | Change::Delete { path } if path == from => {
                    *path = to.to_path_buf();
                }
                _ => {}
            }
        }
    }
}

/// Which stack a finished undo or redo goes on.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Direction {
    Undo,
    Redo,
}

impl PlicApp {
    /// Records changes to the selected clip's segments. Called on render,
    /// which follows every edit.
    pub(super) fn watch_trim(&mut self, cx: &mut Context<Self>) {
        let merging = self.player.read(cx).is_dragging();
        match self.current() {
            Some(entry) => {
                let (path, segments) = (entry.file.path.clone(), entry.segments.clone());
                self.history.watch_trim(&path, &segments, merging);
            }
            None => self.history.unwatch(),
        }
    }

    pub(super) fn undo(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.history.busy {
            return;
        }
        let Some(change) = self.history.undo.pop() else {
            window.push_notification(Notification::info("Nothing to undo."), cx);
            return;
        };
        self.apply(change, Direction::Undo, window, cx);
    }

    pub(super) fn redo(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.history.busy {
            return;
        }
        let Some(change) = self.history.redo.pop() else {
            window.push_notification(Notification::info("Nothing to redo."), cx);
            return;
        };
        self.apply(change, Direction::Redo, window, cx);
    }

    /// Undoes or redoes `change`, then files it on the other stack.
    fn apply(
        &mut self,
        change: Change,
        direction: Direction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match change.clone() {
            Change::Trim {
                path,
                before,
                after,
            } => {
                let segments = if direction == Direction::Undo {
                    before
                } else {
                    after
                };
                if !self.set_segments(&path, segments, window, cx) {
                    self.report_failure(&change, direction, "the clip is gone", window, cx);
                    return;
                }
                self.done(change, direction, window, cx);
            }
            Change::Delete { path } => {
                if direction == Direction::Undo {
                    self.run_file_op(
                        change,
                        direction,
                        move || library::restore_from_trash(&path).map(|()| path.clone()),
                        |this, restored, window, cx| this.refresh_and_select(&restored, window, cx),
                        window,
                        cx,
                    );
                } else {
                    self.release(&path, cx);
                    self.run_file_op(
                        change,
                        direction,
                        move || library::trash_clip(&path).map(|()| path.clone()),
                        |this, path, window, cx| this.forget(&path, window, cx),
                        window,
                        cx,
                    );
                }
            }
            Change::Archive { original, archived } => {
                if direction == Direction::Undo {
                    self.release(&archived, cx);
                    self.run_file_op(
                        change,
                        direction,
                        move || {
                            library::restore_original(&archived)
                                .map(|path| (archived.clone(), path))
                        },
                        |this, (archived, restored), window, cx| {
                            this.history.rename(&archived, &restored);
                            if let Some(index) = this.settled_index_of(&archived) {
                                this.unsettle(index, restored, window, cx);
                            } else {
                                this.refresh_and_select(&restored, window, cx);
                            }
                        },
                        window,
                        cx,
                    );
                } else {
                    self.release(&original, cx);
                    self.run_file_op(
                        change,
                        direction,
                        move || {
                            library::archive_original(&original)
                                .map(|path| (original.clone(), path))
                        },
                        |this, (original, archived), window, cx| {
                            this.history.rename(&original, &archived);
                            if let Some(index) = this.index_of(&original) {
                                let entry = this.take_clip(index, window, cx);
                                this.settle(entry, archived);
                            }
                        },
                        window,
                        cx,
                    );
                }
            }
        }
    }

    /// Runs a file operation off the UI thread, then `finish` with its
    /// result, filing the change on the other stack if it worked.
    fn run_file_op<T: Send + 'static>(
        &mut self,
        change: Change,
        direction: Direction,
        operation: impl FnMut() -> anyhow::Result<T> + Send + 'static,
        finish: impl FnOnce(&mut Self, T, &mut Window, &mut Context<Self>) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.history.busy = true;
        // Retried while a decoder that was just stopped lets go of the file.
        let task = cx.background_spawn(async move { library::retry_while_busy(operation) });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |this, window, cx| {
                this.history.busy = false;
                match result {
                    Ok(value) => {
                        finish(this, value, window, cx);
                        this.done(change, direction, window, cx);
                    }
                    Err(error) => {
                        let reason = format!("{error:#}");
                        this.report_failure(&change, direction, &reason, window, cx);
                        this.show_selected_in_player(cx);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn done(
        &mut self,
        change: Change,
        direction: Direction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let what = change.describe();
        let title = match direction {
            Direction::Undo => {
                self.history.redo.push(change);
                "Undone"
            }
            Direction::Redo => {
                self.history.push_undo(change);
                "Redone"
            }
        };
        window.push_notification(Notification::info(what).title(title), cx);
        cx.notify();
    }

    fn report_failure(
        &mut self,
        change: &Change,
        direction: Direction,
        reason: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let verb = match direction {
            Direction::Undo => "undo",
            Direction::Redo => "redo",
        };
        window.push_notification(
            Notification::error(reason.to_owned())
                .title(format!("Could not {verb} the {}", change.describe())),
            cx,
        );
    }

    /// Puts `segments` on the clip at `path` and selects it.
    fn set_segments(
        &mut self,
        path: &Path,
        segments: Segments,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(index) = self.index_of(path) else {
            return false;
        };
        self.select(index, window, cx);
        self.clips[index].segments = segments.clone();
        self.history.set_watched(path, &segments);
        self.player
            .update(cx, |player, cx| player.set_segments(segments, cx));
        self.sync_time_fields(window, cx);
        cx.notify();
        true
    }

    /// Stops showing `path` in the viewer, which would keep the file open.
    fn release(&mut self, path: &Path, cx: &mut Context<Self>) {
        if self.viewed().is_some_and(|entry| entry.file.path == path) {
            self.player.update(cx, |player, cx| player.clear(cx));
        }
    }

    /// Picks up a file that came back into the folder and selects it.
    fn refresh_and_select(&mut self, path: &Path, window: &mut Window, cx: &mut Context<Self>) {
        self.refresh_folder(window, cx);
        if self.index_of(path).is_none()
            && let Some(folder) = self.folder.clone()
        {
            // Only a full scan picks up originals in the archive folder.
            self.load_folder(folder, window, cx);
        }
        if let Some(index) = self.index_of(path) {
            self.select(index, window, cx);
        } else if let Some(index) = self.settled_index_of(path) {
            self.reveal_settled(index, window, cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::Duration;

    use super::{Change, History};
    use crate::trim::{Segments, TrimRange};

    fn trimmed(start: u64) -> Segments {
        let mut segments = Segments::default();
        segments.set_active(TrimRange::new(Duration::from_secs(start), None));
        segments
    }

    #[test]
    fn records_trim_changes_and_merges_drags() {
        let path = Path::new("a.mp4");
        let mut history = History::default();
        history.watch_trim(path, &Segments::default(), false);
        assert!(history.next_undo().is_none());

        // A drag through several positions is one change.
        history.watch_trim(path, &trimmed(1), true);
        history.watch_trim(path, &trimmed(2), true);
        history.watch_trim(path, &trimmed(3), false);
        assert_eq!(history.undo.len(), 1);
        assert_eq!(
            history.next_undo(),
            Some(&Change::Trim {
                path: path.into(),
                before: Segments::default(),
                after: trimmed(3),
            })
        );

        // The next edit is a change of its own.
        history.watch_trim(path, &trimmed(4), false);
        assert_eq!(history.undo.len(), 2);
    }

    #[test]
    fn switching_clips_is_not_a_change() {
        let mut history = History::default();
        history.watch_trim(Path::new("a.mp4"), &trimmed(1), false);
        history.watch_trim(Path::new("b.mp4"), &trimmed(2), false);
        assert!(history.next_undo().is_none());
    }

    #[test]
    fn a_new_change_clears_redo() {
        let mut history = History::default();
        history.redo.push(Change::Delete {
            path: "a.mp4".into(),
        });
        history.record(Change::Delete {
            path: "b.mp4".into(),
        });
        assert!(history.next_redo().is_none());
    }

    #[test]
    fn follows_renamed_clips() {
        let mut history = History::default();
        history.record(Change::Delete {
            path: "a.mp4".into(),
        });
        history.rename(Path::new("a.mp4"), Path::new("edited/a.mp4"));
        assert_eq!(
            history.next_undo(),
            Some(&Change::Delete {
                path: "edited/a.mp4".into()
            })
        );
    }
}
