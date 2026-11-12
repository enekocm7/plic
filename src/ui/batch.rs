//! Selecting several clips with Ctrl and Shift, and acting on them
//! together: export, move or delete.

use std::path::{Path, PathBuf};

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, WindowExt as _, h_flex};
use gpui_kit::*;

use super::app::{ClipStatus, PlicApp, display_name};
use super::history::Change;
use super::library_panel::List;
use crate::library;

/// Which clips are marked, by path so the marks survive the list changing.
#[derive(Default)]
pub struct Marks {
    paths: Vec<PathBuf>,
    /// Where a Shift range starts.
    anchor: Option<PathBuf>,
}

impl Marks {
    pub fn contains(&self, path: &Path) -> bool {
        self.paths.iter().any(|marked| marked == path)
    }

    pub fn clear(&mut self) {
        self.paths.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }

    /// Adds `path`, or takes it out if it is in already. Returns whether it
    /// is marked now.
    fn toggle(&mut self, path: &Path) -> bool {
        if let Some(position) = self.paths.iter().position(|marked| marked == path) {
            self.paths.remove(position);
            false
        } else {
            self.paths.push(path.to_path_buf());
            true
        }
    }

    /// Marks every path from the anchor to `to` in `rows`, the list as it
    /// is shown. Without an anchor, `to` starts one.
    fn mark_range(&mut self, rows: &[PathBuf], to: &Path) {
        let end = rows.iter().position(|row| row == to);
        let start = self
            .anchor
            .as_deref()
            .and_then(|anchor| rows.iter().position(|row| row == anchor))
            .or(end);
        if let (Some(start), Some(end)) = (start, end) {
            let (low, high) = (start.min(end), start.max(end));
            self.paths = rows[low..=high].to_vec();
        }
    }
}

impl PlicApp {
    /// The clips acted on: the marked ones, or else just the selected one.
    /// In list order.
    pub(super) fn selection(&self) -> Vec<usize> {
        let mut indices: Vec<usize> = self
            .clips
            .iter()
            .enumerate()
            .filter(|(index, entry)| {
                self.marks.contains(&entry.file.path) || Some(*index) == self.selected
            })
            .map(|(index, _)| index)
            .collect();
        if self.marks.is_empty() {
            indices.retain(|index| Some(*index) == self.selected);
        }
        indices
    }

    pub(super) fn is_marked(&self, index: usize) -> bool {
        !self.marks.is_empty()
            && self
                .clips
                .get(index)
                .is_some_and(|entry| self.marks.contains(&entry.file.path))
    }

    /// Whether several clips are selected.
    pub(super) fn has_batch(&self) -> bool {
        self.selection().len() > 1
    }

    /// A click on a clip: on its own it selects just that clip, with Ctrl
    /// it adds or removes the clip, and with Shift it selects the run from
    /// the last clicked one.
    pub(super) fn click_clip(
        &mut self,
        index: usize,
        modifiers: Modifiers,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(path) = self.clips.get(index).map(|entry| entry.file.path.clone()) else {
            return;
        };
        if modifiers.shift {
            if self.marks.anchor.is_none() {
                self.marks.anchor = self.current().map(|entry| entry.file.path.clone());
            }
            let rows: Vec<PathBuf> = self
                .visible(List::Clips, cx)
                .into_iter()
                .map(|row| self.clips[row].file.path.clone())
                .collect();
            self.marks.mark_range(&rows, &path);
            self.select(index, window, cx);
        } else if modifiers.control || modifiers.platform {
            // The clip selected so far becomes part of the selection.
            if self.marks.is_empty()
                && let Some(current) = self.current().map(|entry| entry.file.path.clone())
            {
                self.marks.toggle(&current);
            }
            if self.marks.toggle(&path) {
                self.marks.anchor = Some(path);
                self.select(index, window, cx);
            } else if self.selected == Some(index)
                && let Some(other) = self.selection().first().copied()
            {
                self.select(other, window, cx);
            }
        } else {
            self.marks.clear();
            self.marks.anchor = Some(path);
            self.select(index, window, cx);
        }
        cx.notify();
    }

    /// Marks every clip the list shows.
    pub(super) fn select_all(&mut self, cx: &mut Context<Self>) {
        self.marks.paths = self
            .visible(List::Clips, cx)
            .into_iter()
            .map(|index| self.clips[index].file.path.clone())
            .collect();
        cx.notify();
    }

    /// Back to a single selected clip. Returns whether anything changed.
    pub(super) fn clear_marks(&mut self, cx: &mut Context<Self>) -> bool {
        if self.marks.is_empty() {
            return false;
        }
        self.marks.clear();
        cx.notify();
        true
    }

    /// Queues an export of every selected clip with the export panel's
    /// format and settings, into the selected clip's destination. Names
    /// come from the name template, or each clip's own name.
    pub(super) fn export_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(folder) = self
            .current()
            .and_then(|entry| self.destination(entry))
            .map(Path::to_path_buf)
        else {
            window.push_notification(
                Notification::warning("Create a destination folder first.")
                    .title("Nothing exported"),
                cx,
            );
            return;
        };
        let template = crate::settings::get().name_template;
        let extension = self.editor.format.extension();
        let clips_folder = self.folder.as_deref().map(display_name);
        let mut reserved: Vec<PathBuf> = Vec::new();
        let (mut queued, mut skipped) = (0, Vec::new());
        for index in self.selection() {
            let entry = &self.clips[index];
            let label = entry.file.file_name();
            if entry.status != ClipStatus::Pending {
                skipped.push(format!("{label}: already exported or exporting"));
                continue;
            }
            let taken = |candidate: &str| {
                let output = folder.join(format!("{candidate}.{extension}"));
                output.exists()
                    || reserved.contains(&output)
                    || self.jobs.iter().any(|job| job.output() == output)
            };
            let name = if Some(index) == self.selected {
                self.editor.name(cx)
            } else if !template.is_empty() && (!entry.templated || taken(&entry.name)) {
                let context = crate::naming::TemplateContext {
                    name: &entry.file.stem(),
                    folder: clips_folder.as_deref().unwrap_or_default(),
                    recorded: entry.file.modified,
                };
                crate::naming::expand(&template, &context, taken)
            } else {
                entry.name.clone()
            };
            match self.export_requests_for(index, name, &folder, &reserved) {
                Ok(requests) => {
                    reserved.extend(requests.iter().map(|request| request.output.clone()));
                    self.queue_export(index, requests);
                    queued += 1;
                }
                Err(error) => skipped.push(format!("{label}: {error}")),
            }
        }
        self.start_jobs(window, cx);
        let folder_name = display_name(&folder);
        let title = match queued {
            0 => "Nothing exported".to_owned(),
            1 => format!("Exporting 1 clip to {folder_name}"),
            count => format!("Exporting {count} clips to {folder_name}"),
        };
        let notification = if skipped.is_empty() {
            Notification::success("They run in the background.")
        } else {
            Notification::warning(format!("Skipped:\n{}", skipped.join("\n"))).autohide(false)
        };
        window.push_notification(notification.title(title), cx);
        self.marks.clear();
        cx.notify();
    }

    /// Asks before sending every selected clip to the recycle bin.
    pub(super) fn confirm_delete_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = self.selected_paths_not_exporting(window, cx);
        if paths.is_empty() {
            return;
        }
        let count = paths.len();
        let this = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, cx| {
            let (this, paths) = (this.clone(), paths.clone());
            alert
                .icon(Icon::new(IconName::Trash).text_color(cx.theme().danger))
                .title(format!("Delete {count} clips?"))
                .description(
                    "They go to the Recycle Bin, so you can still restore them from there.",
                )
                .ok_text("Delete")
                .ok_variant(ButtonVariant::Danger)
                .show_cancel(true)
                .on_ok(move |_, window, cx| {
                    let paths = paths.clone();
                    this.update(cx, |this, cx| {
                        this.run_on_files(paths, "delete", library::trash_clip, true, window, cx)
                    })
                    .ok();
                    true
                })
        });
    }

    /// Moves every selected original into `folder`, out of the list.
    pub(super) fn move_selection(
        &mut self,
        folder: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let paths = self.selected_paths_not_exporting(window, cx);
        if paths.is_empty() {
            return;
        }
        self.run_on_files(
            paths,
            "move",
            move |path| library::move_into(path, &folder).map(drop),
            false,
            window,
            cx,
        );
    }

    /// Asks for any folder to move the selection into.
    fn choose_move_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Move clips here".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            if let Some(folder) = paths.into_iter().next() {
                this.update_in(cx, |this, window, cx| {
                    this.move_selection(folder, window, cx)
                })
                .ok();
            }
        })
        .detach();
    }

    /// The selected clips' paths, leaving out any that are exporting.
    fn selected_paths_not_exporting(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<PathBuf> {
        let selection = self.selection();
        let exporting = selection
            .iter()
            .filter(|&&index| self.clips[index].status == ClipStatus::Exporting)
            .count();
        if exporting > 0 {
            window.push_notification(
                Notification::info(format!(
                    "{exporting} of the selected clips are exporting and were left alone."
                )),
                cx,
            );
        }
        selection
            .into_iter()
            .filter(|&index| self.clips[index].status != ClipStatus::Exporting)
            .map(|index| self.clips[index].file.path.clone())
            .collect()
    }

    /// Runs `operation` on each file off the UI thread, then drops the ones
    /// it worked on from the list and reports any it did not. With
    /// `deleting`, each clip that went to the recycle bin can be undone.
    fn run_on_files(
        &mut self,
        paths: Vec<PathBuf>,
        verb: &'static str,
        operation: impl Fn(&Path) -> anyhow::Result<()> + Send + 'static,
        deleting: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The viewer would keep its file open.
        if self
            .viewed()
            .is_some_and(|entry| paths.contains(&entry.file.path))
        {
            self.player.update(cx, |player, cx| player.clear(cx));
        }
        let task = cx.background_spawn(async move {
            paths
                .into_iter()
                .map(|path| {
                    let result = library::retry_while_busy(|| operation(&path));
                    (path, result)
                })
                .collect::<Vec<_>>()
        });
        cx.spawn_in(window, async move |this, cx| {
            let results = task.await;
            this.update_in(cx, |this, window, cx| {
                let mut failures = Vec::new();
                for (path, result) in results {
                    match result {
                        Ok(()) => {
                            if deleting {
                                this.history.record(Change::Delete { path: path.clone() });
                            }
                            this.forget(&path, window, cx)
                        }
                        Err(error) => failures.push(format!("{error:#}")),
                    }
                }
                this.marks.clear();
                if !failures.is_empty() {
                    window.push_notification(
                        Notification::error(failures.join("\n"))
                            .title(format!("Could not {verb} some clips"))
                            .autohide(false),
                        cx,
                    );
                }
                if this.viewed().is_some() {
                    this.show_selected_in_player(cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The bar over the list while several clips are selected.
    pub(super) fn render_batch_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let count = self.selection().len();
        let app = cx.entity().downgrade();
        let folders = self.folders.clone();
        h_flex()
            .flex_none()
            .mx_2()
            .mb_2()
            .px_2()
            .py_1()
            .gap_1()
            .rounded(theme.radius)
            .bg(theme.sidebar_accent)
            .text_color(theme.sidebar_accent_foreground)
            .child(
                div()
                    .flex_1()
                    .text_xs()
                    .font_weight(FontWeight::MEDIUM)
                    .child(format!("{count} selected")),
            )
            .child(
                Button::new("batch-export")
                    .ghost()
                    .xsmall()
                    .icon(IconName::Download)
                    .tooltip("Export the selected clips (Ctrl+Enter)")
                    .on_click(cx.listener(|this, _, window, cx| this.export_selection(window, cx))),
            )
            .child(
                Button::new("batch-move")
                    .ghost()
                    .xsmall()
                    .icon(IconName::FolderInput)
                    .tooltip("Move the selected clips to a folder")
                    .dropdown_menu(move |menu, _, _| {
                        let menu = folders.iter().fold(menu, |menu, folder| {
                            let (app, folder) = (app.clone(), folder.clone());
                            menu.item(PopupMenuItem::new(display_name(&folder)).on_click(
                                move |_, window, cx| {
                                    let folder = folder.clone();
                                    app.update(cx, |this, cx| {
                                        this.move_selection(folder, window, cx)
                                    })
                                    .ok();
                                },
                            ))
                        });
                        let app = app.clone();
                        menu.separator()
                            .item(PopupMenuItem::new("Other folder…").on_click(
                                move |_, window, cx| {
                                    app.update(cx, |this, cx| this.choose_move_folder(window, cx))
                                        .ok();
                                },
                            ))
                    }),
            )
            .child(
                Button::new("batch-delete")
                    .ghost()
                    .xsmall()
                    .icon(IconName::Trash)
                    .tooltip("Delete the selected clips (Delete)")
                    .on_click(
                        cx.listener(|this, _, window, cx| {
                            this.confirm_delete_selection(window, cx)
                        }),
                    ),
            )
            .child(
                Button::new("batch-clear")
                    .ghost()
                    .xsmall()
                    .icon(IconName::X)
                    .tooltip("Select just one clip (Escape)")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.clear_marks(cx);
                    })),
            )
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::Marks;

    fn rows() -> Vec<PathBuf> {
        ["a", "b", "c", "d"].iter().map(PathBuf::from).collect()
    }

    #[test]
    fn shift_marks_the_run_from_the_anchor_either_way() {
        let mut marks = Marks {
            anchor: Some("b".into()),
            ..Marks::default()
        };
        marks.mark_range(&rows(), Path::new("d"));
        assert_eq!(marks.paths, ["b", "c", "d"].map(PathBuf::from));
        marks.mark_range(&rows(), Path::new("a"));
        assert_eq!(marks.paths, ["a", "b"].map(PathBuf::from));
    }

    #[test]
    fn ctrl_toggles_one_clip() {
        let mut marks = Marks::default();
        assert!(marks.toggle(Path::new("a")));
        assert!(marks.toggle(Path::new("c")));
        assert!(!marks.toggle(Path::new("a")));
        assert_eq!(marks.paths, [PathBuf::from("c")]);
    }
}
