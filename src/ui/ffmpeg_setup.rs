//! Downloading plic's own copy of ffmpeg on first run, with a banner that
//! shows the progress and offers a retry if it fails.

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::progress::Progress;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::*;

use super::app::PlicApp;
use crate::ffmpeg::tools;

/// Where the ffmpeg download stands.
pub enum Setup {
    /// Nothing to do: ffmpeg is in place, or comes from the system.
    Idle,
    /// Downloading, with the fraction received so far.
    Downloading(f32),
    Failed(SharedString),
}

impl PlicApp {
    /// Starts downloading ffmpeg if plic does not have its copy yet.
    pub(super) fn set_up_ffmpeg(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.ffmpeg, Setup::Downloading(_)) || !tools::needs_download() {
            return;
        }
        self.ffmpeg = Setup::Downloading(0.0);
        cx.notify();

        let (progress_tx, mut progress_rx) = futures::channel::mpsc::unbounded();
        let download = cx.background_spawn(async move {
            tools::download(|fraction| {
                progress_tx.unbounded_send(fraction).ok();
            })
        });
        cx.spawn_in(window, async move |this, cx| {
            use futures::StreamExt as _;
            // The channel closes when the download returns.
            while let Some(fraction) = progress_rx.next().await {
                let updated = this.update(cx, |this, cx| {
                    // Redraw only on visible steps.
                    if let Setup::Downloading(shown) = &mut this.ffmpeg
                        && (fraction - *shown >= 0.005 || fraction >= 1.0)
                    {
                        *shown = fraction;
                        cx.notify();
                    }
                });
                if updated.is_err() {
                    return;
                }
            }
            let result = download.await;
            this.update_in(cx, |this, window, cx| {
                match result {
                    Ok(()) => {
                        this.ffmpeg = Setup::Idle;
                        crate::ffmpeg::warm_up_encoder(crate::ffmpeg::Codec::default());
                        // Clips listed before ffmpeg was there could not be
                        // read; read them again.
                        this.reload_folder(window, cx);
                    }
                    Err(error) => this.ffmpeg = Setup::Failed(format!("{error:#}").into()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// A bar under the title bar while ffmpeg downloads or after it failed.
    pub(super) fn render_ffmpeg_banner(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let theme = cx.theme();
        let (title, detail, progress, failed): (SharedString, SharedString, Option<f32>, bool) =
            match &self.ffmpeg {
                Setup::Idle => return None,
                Setup::Downloading(fraction) => (
                    "Getting ffmpeg ready".into(),
                    format!(
                        "plic is downloading the ffmpeg it plays and exports with ({:.0}%). This happens once.",
                        fraction * 100.
                    )
                    .into(),
                    Some(*fraction),
                    false,
                ),
                Setup::Failed(error) => (
                    "Could not download ffmpeg".into(),
                    format!("{error}. Retry, or set your own ffmpeg folder in the settings.")
                        .into(),
                    None,
                    true,
                ),
            };
        let accent = if failed { theme.danger } else { theme.primary };
        Some(
            h_flex()
                .id("ffmpeg-banner")
                .flex_none()
                .w_full()
                .px_4()
                .py_2()
                .gap_3()
                .bg(accent.opacity(0.12))
                .border_b_1()
                .border_color(theme.border)
                .child(
                    Icon::new(if failed {
                        IconName::CircleAlert
                    } else {
                        IconName::Download
                    })
                    .small()
                    .text_color(accent),
                )
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_1()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(title),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(detail),
                        )
                        .children(progress.map(|fraction| {
                            Progress::new("ffmpeg-progress").value(fraction * 100.)
                        })),
                )
                .children(failed.then(|| {
                    Button::new("ffmpeg-retry")
                        .primary()
                        .small()
                        .label("Retry")
                        .on_click(cx.listener(|this, _, window, cx| this.set_up_ffmpeg(window, cx)))
                }))
                .into_any_element(),
        )
    }
}
