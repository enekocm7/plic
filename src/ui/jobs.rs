//! The export queue. Exports run one at a time in the background so a
//! hardware encoder is never asked for more sessions than it has; the user
//! moves on to the next clip as soon as one is queued.
//!
//! A clip whose segments are saved as separate files queues one job per
//! segment. Its original is only archived once every one of them succeeded.

use std::path::{Path, PathBuf};

use futures::StreamExt as _;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::progress::Progress;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::app::{ClipStatus, PlicApp};
use crate::{
    ffmpeg::{self, CancelToken, ExportError, ExportRequest},
    library,
};

pub struct ExportJob {
    id: u64,
    /// Shared by the jobs queued for one clip in one go.
    batch: u64,
    request: ExportRequest,
    label: SharedString,
    cancel: CancelToken,
    /// `None` while waiting for an earlier export to finish.
    progress: Option<f32>,
}

impl ExportJob {
    pub fn source(&self) -> &Path {
        &self.request.source
    }

    fn is_running(&self) -> bool {
        self.progress.is_some()
    }
}

impl PlicApp {
    /// Validates the selected clip's draft and queues its export.
    pub(super) fn export_current(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let requests = match self.export_requests(cx) {
            Ok(requests) => requests,
            Err(error) => {
                self.editor.export_error = Some(error);
                cx.notify();
                return;
            }
        };
        self.editor.export_error = None;
        let index = self.selected.expect("export_requests needs a selection");
        self.clips[index].status = ClipStatus::Exporting;

        self.next_job_id += 1;
        let batch = self.next_job_id;
        for request in requests {
            self.next_job_id += 1;
            self.jobs.push(ExportJob {
                id: self.next_job_id,
                batch,
                label: library::display_name(&request.output).into(),
                request,
                cancel: CancelToken::default(),
                progress: None,
            });
        }
        self.start_next_job(window, cx);

        // Keep going with whatever still needs editing.
        if let Some(next) = self.next_pending_after(index) {
            self.select(next, window, cx);
        }
        cx.notify();
    }

    /// One request for the whole clip, or one per segment when they are
    /// saved as separate files.
    fn export_requests(&self, cx: &App) -> Result<Vec<ExportRequest>, SharedString> {
        let entry = self.current().ok_or("Select a clip to export")?;
        if entry.status == ClipStatus::Exporting {
            return Err("This clip is already being exported".into());
        }
        let info = entry.info().ok_or("This clip has not been read yet")?;
        let name = self.editor.name(cx);
        library::validate_file_stem(&name).map_err(|error| error.to_string())?;
        entry.segments.validate(info.duration)?;
        if let Some(target) = self.editor.video.target_size
            && self.editor.mode == ffmpeg::ExportMode::Reencode
            && let Some(length) = entry.segments.total_length(info.duration)
        {
            ffmpeg::target_bitrates(target, length, !entry.export_tracks().is_empty())
                .map_err(|error| format!("{error:#}"))?;
        }
        let folder = self
            .destination(entry)
            .ok_or("Create a destination folder first")?;

        let parts: Vec<(String, Vec<_>)> = if entry.segments.len() == 1 || self.editor.join {
            vec![(name, entry.segments.in_order())]
        } else {
            let parts = entry.segments.named_parts(&name);
            for (index, (part, _)) in parts.iter().enumerate() {
                library::validate_file_stem(part)
                    .map_err(|error| format!("Part {}: {error}", index + 1))?;
                if parts[..index]
                    .iter()
                    .any(|(other, _)| other.eq_ignore_ascii_case(part))
                {
                    return Err(format!("Two parts are called {part}").into());
                }
            }
            parts
                .into_iter()
                .map(|(part, range)| (part, vec![range]))
                .collect()
        };

        parts
            .into_iter()
            .map(|(name, ranges)| {
                let output = folder.join(format!("{name}.mp4"));
                let taken =
                    output.exists() || self.jobs.iter().any(|job| job.request.output == output);
                if taken {
                    return Err(format!(
                        "{} already has a clip called {name}",
                        library::display_name(folder)
                    )
                    .into());
                }
                Ok(ExportRequest {
                    source: entry.file.path.clone(),
                    output,
                    ranges,
                    // Cropping and rotating are filters, so they re-encode.
                    video: self.editor.video,
                    mode: if entry.transform.is_identity() {
                        self.editor.mode
                    } else {
                        ffmpeg::ExportMode::Reencode
                    },
                    source_duration: info.duration,
                    audio_tracks: entry.export_tracks(),
                    audio_adjust: entry.audio_adjust,
                    video_size: info.video_size,
                    transform: entry.transform,
                })
            })
            .collect()
    }

    fn next_pending_after(&self, index: usize) -> Option<usize> {
        let pending = |entry: &&super::app::ClipEntry| entry.status == ClipStatus::Pending;
        let after = self.clips[index + 1..]
            .iter()
            .position(|entry| pending(&entry))
            .map(|offset| index + 1 + offset);
        after.or_else(|| self.clips[..index].iter().position(|entry| pending(&entry)))
    }

    /// Starts the oldest queued export unless one is already running.
    fn start_next_job(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.jobs.iter().any(ExportJob::is_running) {
            return;
        }
        let Some(job) = self.jobs.first_mut() else {
            return;
        };
        job.progress = Some(0.0);
        let (id, request, cancel) = (job.id, job.request.clone(), job.cancel.clone());

        let (progress_tx, mut progress_rx) = futures::channel::mpsc::unbounded();
        let export = cx.background_spawn(async move {
            ffmpeg::export(&request, &cancel, |fraction| {
                progress_tx.unbounded_send(fraction).ok();
            })
        });

        cx.spawn_in(window, async move |this, cx| {
            // The channel closes when the export returns.
            while let Some(fraction) = progress_rx.next().await {
                let updated = this.update(cx, |this, cx| {
                    if let Some(job) = this.jobs.iter_mut().find(|job| job.id == id) {
                        job.progress = Some(fraction);
                        cx.notify();
                    }
                });
                if updated.is_err() {
                    return;
                }
            }
            let result = export.await;
            this.update_in(cx, |this, window, cx| {
                this.finish_job(id, result, window, cx)
            })
            .ok();
        })
        .detach();
    }

    fn finish_job(
        &mut self,
        id: u64,
        result: Result<PathBuf, ExportError>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(position) = self.jobs.iter().position(|job| job.id == id) else {
            return;
        };
        let job = self.jobs.remove(position);
        let index = self.index_of(&job.request.source);
        let batch_done = !self.jobs.iter().any(|other| other.batch == job.batch);
        let succeeded = result.is_ok();
        let batch_failed = if batch_done {
            self.failed_batches.remove(&job.batch)
        } else {
            self.failed_batches.contains(&job.batch)
        };

        match result {
            Ok(output) if batch_done && !batch_failed => {
                self.archive_exported(job, output, window, cx)
            }
            Ok(output) => {
                let folder = output.parent().map(library::display_name);
                window.push_notification(
                    Notification::success(
                        folder.map_or_else(String::new, |folder| format!("Saved to {folder}")),
                    )
                    .title(format!("Exported {}", job.label)),
                    cx,
                );
            }
            Err(error) => {
                if !batch_done {
                    self.failed_batches.insert(job.batch);
                }
                match error {
                    ExportError::Cancelled => window.push_notification(
                        Notification::info(format!("{} was not exported", job.label))
                            .title("Export cancelled"),
                        cx,
                    ),
                    ExportError::Failed(error) => window.push_notification(
                        Notification::error(format!("{error:#}"))
                            .title(format!("Could not export {}", job.label))
                            .autohide(false),
                        cx,
                    ),
                }
            }
        }
        // A clip whose parts did not all export stays to be edited.
        if batch_done
            && (!succeeded || batch_failed)
            && let Some(index) = index
        {
            self.clips[index].status = ClipStatus::Pending;
        }
        self.start_next_job(window, cx);
        cx.notify();
    }

    /// Moves the original of a finished export into `edited/` and drops it
    /// from the library.
    fn archive_exported(
        &mut self,
        job: ExportJob,
        output: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let source = job.request.source;
        // The viewer would keep the file open.
        if self
            .current()
            .is_some_and(|entry| entry.file.path == source)
        {
            self.player.update(cx, |player, cx| player.clear(cx));
        }
        let archive = cx.background_spawn({
            let source = source.clone();
            async move { library::retry_while_busy(|| library::archive_original(&source)) }
        });
        cx.spawn_in(window, async move |this, cx| {
            let archived = archive.await;
            this.update_in(cx, |this, window, cx| {
                let title = format!("Exported {}", job.label);
                let index = this.index_of(&source);
                match archived {
                    Ok(archived) => {
                        let folder = output.parent().map(library::display_name);
                        let message = folder.map_or_else(String::new, |folder| {
                            format!(
                                "Saved to {folder}, original moved to {}",
                                library::EDITED_DIR
                            )
                        });
                        window.push_notification(Notification::success(message).title(title), cx);
                        if let Some(index) = index {
                            let entry = this.take_clip(index, window, cx);
                            this.settle(entry, archived);
                        }
                    }
                    Err(error) => {
                        window.push_notification(
                            Notification::warning(format!(
                                "Could not move the original: {error:#}"
                            ))
                            .title(title),
                            cx,
                        );
                        if let Some(index) = index {
                            this.clips[index].status = ClipStatus::Exported;
                            // Show it again if it was cleared above.
                            if this.selected == Some(index) {
                                this.show_selected_in_player(cx);
                            }
                            cx.notify();
                        }
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    fn cancel_job(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(position) = self.jobs.iter().position(|job| job.id == id) else {
            return;
        };
        if self.jobs[position].is_running() {
            // The export task notices, cleans up and reports back.
            self.jobs[position].cancel.cancel();
            return;
        }
        let job = self.jobs.remove(position);
        if let Some(index) = self.index_of(&job.request.source) {
            self.clips[index].status = ClipStatus::Pending;
        }
        self.start_next_job(window, cx);
        cx.notify();
    }

    pub(super) fn render_queue(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        v_flex().when(!self.jobs.is_empty(), |queue| {
            queue
                .flex_none()
                .max_h(px(260.))
                .border_t_1()
                .border_color(theme.sidebar_border)
                .child(
                    h_flex().h(px(36.)).px_4().justify_between().child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.muted_foreground)
                            .child(format!("QUEUE · {}", self.jobs.len())),
                    ),
                )
                .child(
                    v_flex()
                        .id("export-queue")
                        .overflow_y_scroll()
                        .px_2()
                        .pb_2()
                        .gap_1()
                        .children(self.jobs.iter().map(|job| self.render_job(job, cx))),
                )
        })
    }

    fn render_job(&self, job: &ExportJob, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let id = job.id;
        let status = match job.progress {
            Some(_) if job.cancel.is_cancelled() => "Cancelling…".to_string(),
            Some(fraction) => format!("{:.0}%", fraction * 100.0),
            None => "Waiting".to_string(),
        };
        v_flex()
            .p_2()
            .gap_1p5()
            .rounded(theme.radius)
            .bg(theme.muted)
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Icon::new(if job.is_running() {
                            IconName::LoaderCircle
                        } else {
                            IconName::Clock
                        })
                        .xsmall()
                        .text_color(theme.muted_foreground),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .truncate()
                            .child(job.label.clone()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(status),
                    )
                    .child(
                        Button::new(("cancel-job", id))
                            .ghost()
                            .xsmall()
                            .icon(IconName::X)
                            .tooltip("Cancel this export")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.cancel_job(id, window, cx)
                            })),
                    ),
            )
            .child(Progress::new(("job-progress", id)).value(job.progress.unwrap_or(0.0) * 100.0))
    }
}
