//! The keyboard shortcuts reference, opened with `?` or F1. It lists the
//! keys in effect, including any changed in the settings.

use gpui_kit::component::kbd::Kbd;
use gpui_kit::component::{ActiveTheme as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::keymap::{self, Command};

/// A titled list of commands and the keys bound to them.
type Group = (&'static str, Vec<(&'static Command, Vec<String>)>);

fn groups() -> Vec<Group> {
    let bindings = keymap::effective();
    keymap::GROUPS
        .iter()
        .map(|&group| {
            let entries = bindings
                .iter()
                .filter(|(command, _)| command.group == group)
                .cloned()
                .collect();
            (group, entries)
        })
        .collect()
}

pub fn open(window: &mut Window, cx: &mut App) {
    window.open_dialog(cx, |dialog, _, cx| {
        let muted = cx.theme().muted_foreground;
        let mut groups = groups();
        let right = groups.split_off(2);
        dialog.title("Keyboard shortcuts").w(px(660.)).child(
            v_flex()
                .gap_4()
                .child(
                    h_flex()
                        .items_start()
                        .gap_8()
                        .child(column(groups, muted))
                        .child(column(right, muted)),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child("Change these under Settings (Ctrl+,)."),
                ),
        )
    });
}

fn column(groups: Vec<Group>, heading: Hsla) -> impl IntoElement {
    v_flex()
        .flex_1()
        .gap_5()
        .children(groups.into_iter().map(|(title, entries)| {
            v_flex()
                .gap_2()
                .child(
                    div()
                        .text_xs()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(heading)
                        .child(title.to_uppercase()),
                )
                .children(entries.into_iter().map(|(command, keys)| {
                    h_flex()
                        .gap_4()
                        .justify_between()
                        .text_sm()
                        .child(command.description)
                        .child(
                            h_flex()
                                .gap_1()
                                .flex_none()
                                .when(keys.is_empty(), |row| {
                                    row.child(div().text_xs().text_color(heading).child("none"))
                                })
                                .children(
                                    keys.iter()
                                        .filter_map(|keys| keymap::parse_keys(keys))
                                        .flatten()
                                        .map(Kbd::new),
                                ),
                        )
                }))
        }))
}
