//! The clip list on the left of the window.

use std::ops::Range;

use gpui_kit::assets::IconName;
use gpui_kit::component::scroll::Scrollbar;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, Theme, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::app::{ClipEntry, ClipStatus, Details, PlicApp};
use crate::timestamp;

const PANEL_WIDTH: f32 = 280.;
const ROW_HEIGHT: f32 = 64.;

impl PlicApp {
    pub(super) fn render_library(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let count = self.clips.len();
        v_flex()
            .w(px(PANEL_WIDTH))
            .flex_none()
            .h_full()
            .bg(theme.sidebar)
            .border_r_1()
            .border_color(theme.sidebar_border)
            .child(
                h_flex()
                    .h(px(40.))
                    .px_3()
                    .flex_none()
                    .justify_between()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.muted_foreground)
                            .child("CLIPS"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(count.to_string()),
                    ),
            )
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(
                        uniform_list("clips", count, cx.processor(Self::render_clip_rows))
                            .size_full()
                            .track_scroll(&self.list_scroll),
                    )
                    .child(Scrollbar::vertical(&self.list_scroll)),
            )
    }

    fn render_clip_rows(
        &mut self,
        range: Range<usize>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let theme = cx.theme().clone();
        range
            .filter_map(|index| {
                let entry = self.clips.get(index)?;
                let selected = self.selected == Some(index);
                let row = h_flex()
                    .id(("clip", index))
                    .size_full()
                    .px_2()
                    .gap_3()
                    .rounded(theme.radius)
                    .cursor_pointer()
                    .when(selected, |row| row.bg(theme.sidebar_accent))
                    .when(!selected, |row| row.hover(|row| row.bg(theme.list_hover)))
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.select(index, window, cx)),
                    )
                    .child(render_thumbnail(entry, &theme))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_0p5()
                            .child(
                                div()
                                    .text_sm()
                                    .truncate()
                                    .when(selected, |text| {
                                        text.text_color(theme.sidebar_accent_foreground)
                                    })
                                    .child(entry.file.file_name()),
                            )
                            .child(match entry.status {
                                ClipStatus::Pending => div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .truncate()
                                    .child(subtitle(entry))
                                    .into_any_element(),
                                ClipStatus::Exporting => render_status(
                                    IconName::LoaderCircle,
                                    "Exporting",
                                    theme.primary,
                                ),
                                ClipStatus::Exported => {
                                    render_status(IconName::CircleCheck, "Exported", theme.success)
                                }
                            }),
                    );
                // Padding outside the row keeps the highlight off the panel edges.
                Some(
                    div()
                        .h(px(ROW_HEIGHT))
                        .px_2()
                        .py_0p5()
                        .child(row)
                        .into_any_element(),
                )
            })
            .collect()
    }
}

fn render_thumbnail(entry: &ClipEntry, theme: &Theme) -> impl IntoElement {
    let duration = entry
        .info()
        .and_then(|info| info.duration)
        .map(timestamp::format_coarse);
    div()
        .relative()
        .w(px(80.))
        .h(px(45.))
        .flex_none()
        .rounded(theme.radius)
        .overflow_hidden()
        .bg(theme.muted)
        .flex()
        .items_center()
        .justify_center()
        .map(|frame| match (&entry.thumbnail, &entry.details) {
            (Some(thumbnail), _) => frame.child(
                img(thumbnail.clone())
                    .size_full()
                    .object_fit(ObjectFit::Cover),
            ),
            (None, Details::Failed(_)) => frame.child(
                Icon::new(IconName::CircleAlert)
                    .small()
                    .text_color(theme.danger),
            ),
            (None, _) => frame.child(
                Icon::new(IconName::Film)
                    .small()
                    .text_color(theme.muted_foreground),
            ),
        })
        .when_some(duration, |frame, duration| {
            frame.child(
                div()
                    .absolute()
                    .right(px(3.))
                    .bottom(px(3.))
                    .px_1()
                    .rounded_sm()
                    .bg(black().opacity(0.75))
                    .text_color(white())
                    .text_size(px(10.))
                    .line_height(px(14.))
                    .child(duration),
            )
        })
}

fn render_status(icon: IconName, label: &'static str, color: Hsla) -> AnyElement {
    h_flex()
        .gap_1()
        .text_xs()
        .text_color(color)
        .child(Icon::new(icon).xsmall())
        .child(label)
        .into_any_element()
}

/// `1080p60 · 22 MB`, or why the clip could not be read.
fn subtitle(entry: &ClipEntry) -> SharedString {
    let size = format_size(entry.file.size);
    match &entry.details {
        Details::Ready(info) => match info.video_size {
            Some(video) => {
                let rate = info
                    .frame_rate
                    .map(|rate| format!("{}", rate.round() as u32))
                    .unwrap_or_default();
                format!("{}p{rate} · {size}", video.width.min(video.height)).into()
            }
            None => format!("Audio only · {size}").into(),
        },
        Details::Loading => size.into(),
        Details::Failed(_) => "Unreadable video".into(),
    }
}

/// Human-readable file size such as `12.4 MB`.
pub(super) fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    if value < 1024.0 {
        return format!("{bytes} B");
    }
    let mut unit = "B";
    for next in UNITS {
        value /= 1024.0;
        unit = next;
        if value < 1024.0 {
            break;
        }
    }
    if value < 10.0 {
        format!("{value:.1} {unit}")
    } else {
        format!("{value:.0} {unit}")
    }
}

#[cfg(test)]
mod tests {
    use super::format_size;

    #[test]
    fn formats_sizes_with_sensible_precision() {
        assert_eq!(format_size(512), "512 B");
        assert_eq!(format_size(1536), "1.5 KB");
        assert_eq!(format_size(25 * 1024 * 1024), "25 MB");
        assert_eq!(format_size(3 * 1024 * 1024 * 1024 / 2), "1.5 GB");
    }
}
