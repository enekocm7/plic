//! The keyboard shortcuts reference, opened with `?` or F1. It lists the
//! keys in effect, including any changed in the settings, grouped by what
//! they do and filtered by a search box.

use std::rc::Rc;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::Button;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::kbd::Kbd;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::keymap::{self, Command};

/// Called when the user asks to change the keys.
pub type OnCustomize = Rc<dyn Fn(&mut Window, &mut App)>;

/// The icon shown beside a group's heading.
pub fn group_icon(group: &str) -> IconName {
    match group {
        "Playback" => IconName::Play,
        "Timeline" => IconName::Clock,
        "Trimming" => IconName::Scissors,
        "Export" => IconName::Download,
        "Library" => IconName::Folder,
        _ => IconName::Settings,
    }
}

/// Whether every word of `query` appears in the command's description,
/// its group or one of its keys. `query` is expected in lower case.
pub fn matches(query: &str, command: &Command, keys: &[String]) -> bool {
    let haystack = format!(
        "{} {} {}",
        command.description,
        command.group,
        keys.join(" ")
    )
    .to_lowercase();
    query.split_whitespace().all(|word| haystack.contains(word))
}

/// Key caps for each key sequence, separated by "or". Fixed keys are drawn
/// outlined.
pub fn keycaps(keys: &[String], fixed: &[&str], muted: Hsla) -> impl IntoElement {
    let mut row = h_flex().gap_1().flex_none().flex_wrap().justify_end();
    let all = keys
        .iter()
        .map(|keys| (keys.as_str(), false))
        .chain(fixed.iter().map(|keys| (*keys, true)));
    let mut first = true;
    for (keys, fixed) in all {
        let Some(strokes) = keymap::parse_keys(keys) else {
            continue;
        };
        if !first {
            row = row.child(div().text_xs().text_color(muted).child("or"));
        }
        first = false;
        row = row.child(
            h_flex().gap_0p5().children(
                strokes
                    .into_iter()
                    .map(|stroke| Kbd::new(stroke).when(fixed, |kbd| kbd.outline())),
            ),
        );
    }
    if first {
        row = row.child(
            div()
                .text_xs()
                .italic()
                .text_color(muted)
                .child("Not bound"),
        );
    }
    row
}

/// A titled card for one group of shortcuts.
pub fn group_card(
    group: &'static str,
    count: usize,
    rows: impl IntoIterator<Item = AnyElement>,
    cx: &App,
) -> Div {
    let theme = cx.theme();
    v_flex()
        .w_full()
        .border_1()
        .border_color(theme.border)
        .rounded(theme.radius)
        .overflow_hidden()
        .child(
            h_flex()
                .gap_2()
                .px_3()
                .py_2()
                .bg(theme.muted)
                .border_b_1()
                .border_color(theme.border)
                .child(
                    Icon::new(group_icon(group))
                        .small()
                        .text_color(theme.muted_foreground),
                )
                .child(
                    div()
                        .flex_1()
                        .text_xs()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(group.to_uppercase()),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(count.to_string()),
                ),
        )
        .child(v_flex().py_1().children(rows))
}

/// The groups that have commands matching `query`, with those commands and
/// their keys.
/// A group's heading and its commands with their keys.
type Group = (&'static str, Vec<(&'static Command, Vec<String>)>);

fn filtered(query: &str) -> Vec<Group> {
    let bindings = keymap::effective();
    keymap::GROUPS
        .iter()
        .map(|&group| {
            let entries: Vec<_> = bindings
                .iter()
                .filter(|(command, keys)| {
                    command.group == group && {
                        let mut all = keys.clone();
                        all.extend(keymap::fixed_keys(command).map(str::to_owned));
                        matches(query, command, &all)
                    }
                })
                .cloned()
                .collect();
            (group, entries)
        })
        .filter(|(_, entries)| !entries.is_empty())
        .collect()
}

/// Splits groups into two columns of about the same height, keeping their
/// order down each column.
pub fn balance<T>(groups: Vec<(T, usize)>) -> (Vec<T>, Vec<T>) {
    let total: usize = groups.iter().map(|(_, rows)| rows + 2).sum();
    let mut left = Vec::new();
    let mut right = Vec::new();
    let mut height = 0;
    for (group, rows) in groups {
        if height < total / 2 || left.is_empty() {
            height += rows + 2;
            left.push(group);
        } else {
            right.push(group);
        }
    }
    (left, right)
}

pub struct ShortcutsView {
    search: Entity<InputState>,
    on_customize: OnCustomize,
    _subscription: Subscription,
}

impl ShortcutsView {
    fn new(on_customize: OnCustomize, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Search by action or key, e.g. trim or ctrl")
        });
        let subscription = cx.subscribe(&search, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        });
        Self {
            search,
            on_customize,
            _subscription: subscription,
        }
    }
}

impl Render for ShortcutsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let query = self.search.read(cx).value().trim().to_lowercase();
        let groups = filtered(&query);
        let muted = cx.theme().muted_foreground;
        let hover = cx.theme().list_hover;
        let shown: usize = groups.iter().map(|(_, entries)| entries.len()).sum();
        let cards: Vec<(AnyElement, usize)> = groups
            .into_iter()
            .map(|(group, entries)| {
                let count = entries.len();
                let rows = entries.into_iter().map(|(command, keys)| {
                    let fixed: Vec<&str> = keymap::fixed_keys(command).collect();
                    h_flex()
                        .id(command.id)
                        .gap_4()
                        .px_3()
                        .py_1p5()
                        .justify_between()
                        .hover(|row| row.bg(hover))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_sm()
                                .child(command.description),
                        )
                        .child(keycaps(&keys, &fixed, muted))
                        .into_any_element()
                });
                (group_card(group, count, rows, cx).into_any_element(), count)
            })
            .collect();
        let (left, right) = balance(cards);
        let list_height = (window.viewport_size().height - px(250.)).clamp(px(200.), px(560.));
        let on_customize = self.on_customize.clone();
        v_flex()
            .gap_3()
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div().flex_1().child(
                            Input::new(&self.search)
                                .prefix(Icon::new(IconName::Search).small().text_color(muted))
                                .cleanable(true),
                        ),
                    )
                    .child(
                        Button::new("shortcuts-customize")
                            .outline()
                            .icon(IconName::Keyboard)
                            .label("Customize")
                            .on_click(move |_, window, cx| {
                                window.close_dialog(cx);
                                // Open settings once this dialog is gone,
                                // so closing it does not close them too.
                                let on_customize = on_customize.clone();
                                window.defer(cx, move |window, cx| on_customize(window, cx));
                            }),
                    ),
            )
            .child(
                div()
                    .id("shortcuts-list")
                    .max_h(list_height)
                    .overflow_y_scroll()
                    .pr_1()
                    .map(|list| {
                        if shown == 0 {
                            list.child(
                                v_flex()
                                    .w_full()
                                    .py_12()
                                    .items_center()
                                    .justify_center()
                                    .gap_2()
                                    .text_color(muted)
                                    .child(Icon::new(IconName::Search).large())
                                    .child(
                                        div()
                                            .text_sm()
                                            .child(format!("No shortcuts match \u{201c}{query}\u{201d}")),
                                    ),
                            )
                        } else {
                            list.child(
                                h_flex()
                                    .items_start()
                                    .gap_3()
                                    .child(v_flex().flex_1().min_w_0().gap_3().children(left))
                                    .child(v_flex().flex_1().min_w_0().gap_3().children(right)),
                            )
                        }
                    }),
            )
            .child(
                h_flex()
                    .gap_1()
                    .text_xs()
                    .text_color(muted)
                    .child(Icon::new(IconName::Keyboard).xsmall())
                    .child(
                        "Outlined keys are built in and also work while typing in a field. Single keys don't fire while a field has focus.",
                    ),
            )
    }
}

pub fn open(on_customize: OnCustomize, window: &mut Window, cx: &mut App) {
    let view = cx.new(|cx| ShortcutsView::new(on_customize, window, cx));
    let focus = view.read(cx).search.read(cx).focus_handle(cx);
    window.defer(cx, move |window, cx| focus.focus(window, cx));
    window.open_dialog(cx, move |dialog, _, _| {
        dialog
            .title("Keyboard shortcuts")
            .w(px(780.))
            .child(view.clone())
    });
}
