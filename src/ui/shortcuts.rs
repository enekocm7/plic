//! The keyboard shortcuts reference, opened with `?` or F1.

use gpui_kit::component::kbd::Kbd;
use gpui_kit::component::{ActiveTheme as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::*;

/// A titled list of shortcuts: the keystrokes and what they do.
type Group = (
    &'static str,
    &'static [(&'static [&'static str], &'static str)],
);

/// Shortcuts grouped by what they act on. Each entry lists alternative
/// keystrokes for the same command.
const GROUPS: &[Group] = &[
    (
        "Playback",
        &[
            (&["space", "k"], "Play or pause"),
            (&["j"], "Play backward; press again to go faster"),
            (&["l"], "Play forward; press again to go faster"),
            (&["left", "right"], "Back or forward by the seek step"),
            (&[",", "."], "Previous or next frame"),
            (&["home", "end"], "Start or end of the clip"),
            (&["m"], "Mute"),
            (&["b"], "Add or remove a marker"),
            (&["[", "]"], "Previous or next marker"),
            (&["shift-b"], "Trim to the markers around the playhead"),
            (&["f", "f11"], "Fullscreen viewer"),
            (&["escape"], "Leave fullscreen"),
        ],
    ),
    (
        "Trimming",
        &[
            (&["i"], "Start at the playhead"),
            (&["o"], "End at the playhead"),
            (&["shift-i", "shift-o"], "Jump to start or end"),
            (&["x"], "Reset to the whole clip"),
            (&["n"], "Add a segment at the playhead"),
            (&["s"], "Split the segment at the playhead"),
            (&["r"], "Loop the selection"),
        ],
    ),
    (
        "Clips",
        &[
            (&["up", "down"], "Previous or next clip"),
            (&["ctrl-enter"], "Export"),
            (&["ctrl-shift-c"], "Copy the last export to the clipboard"),
            (
                &["ctrl-shift-s", "ctrl-alt-s"],
                "Save the frame as PNG or JPEG",
            ),
            (&["delete"], "Delete"),
            (&["ctrl-o"], "Open a folder"),
            (&["f5"], "Rescan the folder"),
            (&["ctrl-t"], "Light or dark theme"),
            (&["ctrl-,"], "Settings"),
        ],
    ),
];

pub fn open(window: &mut Window, cx: &mut App) {
    window.open_dialog(cx, |dialog, _, cx| {
        let theme = cx.theme();
        dialog.title("Keyboard shortcuts").w(px(620.)).child(
            h_flex()
                .items_start()
                .gap_8()
                .child(column(&GROUPS[..2], theme.muted_foreground))
                .child(column(&GROUPS[2..], theme.muted_foreground)),
        )
    });
}

fn column(groups: &'static [Group], heading: Hsla) -> impl IntoElement {
    v_flex()
        .flex_1()
        .gap_5()
        .children(groups.iter().map(|(title, entries)| {
            v_flex()
                .gap_2()
                .child(
                    div()
                        .text_xs()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(heading)
                        .child(title.to_uppercase()),
                )
                .children(entries.iter().map(|(keys, description)| {
                    h_flex()
                        .gap_4()
                        .justify_between()
                        .text_sm()
                        .child(*description)
                        .child(
                            h_flex()
                                .gap_1()
                                .flex_none()
                                .children(keys.iter().map(|key| {
                                    Kbd::new(
                                        Keystroke::parse(key)
                                            .expect("shortcut keystrokes are valid"),
                                    )
                                })),
                        )
                }))
        }))
}
