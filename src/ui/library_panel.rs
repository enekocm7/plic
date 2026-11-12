//! The clip list on the left of the window.

use std::ops::Range;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::Input;
use gpui_kit::component::scroll::Scrollbar;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, Theme, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::OpenFolder;
use super::app::{ClipEntry, ClipStatus, Details, PlicApp};
use crate::clip_order::SortKey;
use crate::clip_order::{SortItem, StatusFilter, visible as visible_rows};
use crate::{library, timestamp};

const PANEL_WIDTH: f32 = 280.;
const ROW_HEIGHT: f32 = 64.;
/// The open Settled list grows up to this many rows, then scrolls.
const SETTLED_VISIBLE_ROWS: f32 = 4.5;

/// Which of the two lists a row belongs to.
#[derive(Clone, Copy)]
pub(super) enum List {
    Clips,
    Settled,
}

impl PlicApp {
    /// Indices of the clips a list shows, in the order they are shown.
    pub(super) fn visible(&self, list: List, cx: &App) -> Vec<usize> {
        let entries = match list {
            List::Clips => &self.clips,
            List::Settled => &self.settled,
        };
        let names: Vec<String> = entries.iter().map(|entry| entry.file.file_name()).collect();
        let items: Vec<SortItem> = entries
            .iter()
            .zip(&names)
            .map(|(entry, name)| SortItem {
                name,
                modified: entry.file.modified,
                duration: entry.info().and_then(|info| info.duration),
                size: entry.file.size,
            })
            .collect();
        visible_rows(
            &items,
            &self.filter.query(cx),
            self.filter.sort,
            self.filter.descending,
        )
    }

    fn render_filter_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let filter = &self.filter;
        v_flex()
            .flex_none()
            .px_3()
            .pb_2()
            .gap_1p5()
            .child(Input::new(&filter.search).small().cleanable(true))
            .child(
                h_flex()
                    .gap_1()
                    .child(
                        Button::new("sort-key")
                            .ghost()
                            .xsmall()
                            .label(format!("Sort: {}", filter.sort.label()))
                            .tooltip("Change what the list is sorted by")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.filter.sort = this.filter.sort.next();
                                this.filter.descending = this.filter.sort != SortKey::Name
                                    && this.filter.sort != SortKey::Size;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("sort-direction")
                            .ghost()
                            .xsmall()
                            .icon(if filter.descending {
                                IconName::ArrowDown
                            } else {
                                IconName::ArrowUp
                            })
                            .tooltip("Reverse the order")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.filter.descending = !this.filter.descending;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("status-filter")
                            .ghost()
                            .xsmall()
                            .label(format!("Show: {}", filter.status.label()))
                            .tooltip("Show new clips, edited ones or both")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.filter.status = this.filter.status.next();
                                cx.notify();
                            })),
                    ),
            )
    }

    pub(super) fn render_library(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let total = self.clips.len();
        let show_clips = self.filter.status != StatusFilter::Edited;
        let show_settled = self.filter.status != StatusFilter::New;
        let count = if show_clips {
            self.visible(List::Clips, cx).len()
        } else {
            0
        };
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
                    .child(div().text_xs().text_color(theme.muted_foreground).child(
                        if count == total {
                            count.to_string()
                        } else {
                            format!("{count} of {total}")
                        },
                    )),
            )
            .child(self.render_filter_bar(cx))
            .map(|panel| {
                if !show_clips {
                    return panel.child(div().flex_1());
                }
                if total == 0 {
                    return panel.child(self.render_empty_library(cx));
                }
                if count == 0 {
                    return panel.child(
                        div()
                            .flex_1()
                            .p_6()
                            .text_center()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child("No clips match the search."),
                    );
                }
                panel.child(
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
            })
            .when(show_settled && !self.settled.is_empty(), |panel| {
                panel.child(self.render_settled(cx))
            })
    }

    /// The collapsible list of originals already exported, at the bottom.
    fn render_settled(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let count = self.visible(List::Settled, cx).len();
        // Looking only at edited clips has nothing else to show.
        let open = self.settled_open || self.filter.status == StatusFilter::Edited;
        let height = px(ROW_HEIGHT * (count as f32).min(SETTLED_VISIBLE_ROWS) + 4.);
        v_flex()
            .flex_none()
            .border_t_1()
            .border_color(theme.sidebar_border)
            .child(
                h_flex()
                    .id("settled-toggle")
                    .h(px(36.))
                    .mx_2()
                    .my_1()
                    .px_2()
                    .gap_2()
                    .rounded(theme.radius)
                    .cursor_pointer()
                    .text_color(theme.muted_foreground)
                    .hover(|row| row.bg(theme.list_hover).text_color(theme.foreground))
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_settled(cx)))
                    .child(Icon::new(IconName::Archive).small())
                    .child(div().flex_1().text_sm().child("Settled"))
                    .child(div().text_xs().child(count.to_string()))
                    .child(
                        Icon::new(if open {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        })
                        .small(),
                    ),
            )
            .when(open, |section| {
                section.child(
                    div()
                        .relative()
                        .h(height)
                        .pb_1()
                        .child(
                            uniform_list("settled", count, cx.processor(Self::render_settled_rows))
                                .size_full()
                                .track_scroll(&self.settled_scroll),
                        )
                        .child(Scrollbar::vertical(&self.settled_scroll)),
                )
            })
    }

    /// Explains why the list is empty and offers a way forward.
    fn render_empty_library(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (icon, title, message): (_, &str, SharedString) = match (&self.folder, &self.load_error)
        {
            (None, _) => (
                IconName::FolderOpen,
                "No folder open",
                "Open the folder your recordings are saved to.".into(),
            ),
            (Some(_), Some(error)) => (
                IconName::CircleAlert,
                "Could not read this folder",
                error.clone(),
            ),
            (Some(_), None) if !self.settled.is_empty() => (
                IconName::CircleCheck,
                "All caught up",
                "Every clip here has been edited. New recordings will show up here.".into(),
            ),
            (Some(_), None) => (
                IconName::Clapperboard,
                "No clips here",
                format!(
                    "Videos saved directly in this folder show up here. \
                     Originals move to the \"{}\" folder once exported.",
                    library::EDITED_DIR
                )
                .into(),
            ),
        };
        v_flex()
            .flex_1()
            .items_center()
            .justify_center()
            .gap_2()
            .px_6()
            .pb_10()
            .text_center()
            .child(Icon::new(icon).large().text_color(theme.muted_foreground))
            .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(title))
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(message),
            )
            .child(
                Button::new("empty-open-folder")
                    .outline()
                    .small()
                    .mt_2()
                    .icon(IconName::FolderOpen)
                    .label("Open folder")
                    .on_click(|_, window, cx| window.dispatch_action(Box::new(OpenFolder), cx)),
            )
    }

    fn render_clip_rows(
        &mut self,
        range: Range<usize>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        self.render_rows(List::Clips, range, cx)
    }

    fn render_settled_rows(
        &mut self,
        range: Range<usize>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        self.render_rows(List::Settled, range, cx)
    }

    fn render_rows(
        &self,
        list: List,
        range: Range<usize>,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let theme = cx.theme().clone();
        let rows = self.visible(list, cx);
        let (entries, selected_index, id) = match list {
            List::Clips => (&self.clips, self.selected, "clip"),
            List::Settled => (&self.settled, self.settled_selected, "settled-clip"),
        };
        range
            .filter_map(|row| {
                let index = *rows.get(row)?;
                let entry = entries.get(index)?;
                let selected = selected_index == Some(index);
                let row = h_flex()
                    .id((id, index))
                    .size_full()
                    .px_2()
                    .gap_3()
                    .rounded(theme.radius)
                    .cursor_pointer()
                    .when(selected, |row| row.bg(theme.sidebar_accent))
                    .when(!selected, |row| row.hover(|row| row.bg(theme.list_hover)))
                    .on_click(cx.listener(move |this, _, window, cx| match list {
                        List::Clips => this.select(index, window, cx),
                        List::Settled => this.select_settled(index, window, cx),
                    }))
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
