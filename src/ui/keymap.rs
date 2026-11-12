//! The commands that have keyboard shortcuts, their default keys, and the
//! bindings in effect once the user's settings are applied.

use std::{cell::RefCell, collections::BTreeMap, rc::Rc};

use gpui_kit::*;

use super::*;

/// Where a shortcut works.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Scope {
    /// Only while no text field has focus: single keys like `k` or `i`.
    Keys,
    /// Anywhere in the main window, text fields included.
    Window,
    /// Anywhere in the app.
    App,
}

/// A command that can be bound to keys.
pub struct Command {
    /// The name used in `settings.json`.
    pub id: &'static str,
    pub group: &'static str,
    pub description: &'static str,
    pub defaults: &'static [&'static str],
    scope: Scope,
    action: fn() -> Box<dyn Action>,
}

/// The headings commands are grouped under, in the order shown.
pub const GROUPS: [&str; 3] = ["Playback", "Trimming", "Clips"];

macro_rules! command {
    ($id:literal, $group:literal, $action:ident, $scope:ident, [$($key:literal),*], $description:literal) => {
        Command {
            id: $id,
            group: $group,
            description: $description,
            defaults: &[$($key),*],
            scope: Scope::$scope,
            action: || Box::new($action),
        }
    };
}

pub const COMMANDS: &[Command] = &[
    command!(
        "toggle_play",
        "Playback",
        TogglePlay,
        Keys,
        ["space", "k"],
        "Play or pause"
    ),
    command!(
        "seek_backward",
        "Playback",
        SeekBackward,
        Keys,
        ["left"],
        "Seek back"
    ),
    command!(
        "shuttle_backward",
        "Playback",
        ShuttleBackward,
        Keys,
        ["j"],
        "Play backward; press again to go faster"
    ),
    command!(
        "seek_forward",
        "Playback",
        SeekForward,
        Keys,
        ["right"],
        "Seek forward"
    ),
    command!(
        "shuttle_forward",
        "Playback",
        ShuttleForward,
        Keys,
        ["l"],
        "Play forward; press again to go faster"
    ),
    command!(
        "zoom_in",
        "Playback",
        ZoomIn,
        Keys,
        ["=", "+"],
        "Zoom the timeline in"
    ),
    command!(
        "zoom_out",
        "Playback",
        ZoomOut,
        Keys,
        ["-"],
        "Zoom the timeline out"
    ),
    command!(
        "zoom_to_fit",
        "Playback",
        ZoomToFit,
        Keys,
        ["0"],
        "Show the whole clip on the timeline"
    ),
    command!(
        "step_backward",
        "Playback",
        StepBackward,
        Keys,
        [","],
        "Previous frame"
    ),
    command!(
        "step_forward",
        "Playback",
        StepForward,
        Keys,
        ["."],
        "Next frame"
    ),
    command!(
        "jump_to_start",
        "Playback",
        JumpToStart,
        Keys,
        ["home"],
        "Start of the clip"
    ),
    command!(
        "jump_to_end",
        "Playback",
        JumpToEnd,
        Keys,
        ["end"],
        "End of the clip"
    ),
    command!("toggle_mute", "Playback", ToggleMute, Keys, ["m"], "Mute"),
    command!(
        "toggle_marker",
        "Playback",
        ToggleMarker,
        Keys,
        ["b"],
        "Add or remove a marker"
    ),
    command!(
        "previous_marker",
        "Playback",
        PreviousMarker,
        Keys,
        ["["],
        "Previous marker"
    ),
    command!(
        "next_marker",
        "Playback",
        NextMarker,
        Keys,
        ["]"],
        "Next marker"
    ),
    command!(
        "trim_to_markers",
        "Playback",
        TrimToMarkers,
        Keys,
        ["shift-b"],
        "Trim to the markers around the playhead"
    ),
    command!(
        "toggle_fullscreen",
        "Playback",
        ToggleFullscreen,
        Keys,
        ["f"],
        "Fullscreen viewer"
    ),
    command!(
        "exit_fullscreen",
        "Playback",
        ExitFullscreen,
        Keys,
        ["escape"],
        "Leave fullscreen"
    ),
    command!(
        "mark_in",
        "Trimming",
        MarkIn,
        Keys,
        ["i"],
        "Start at the playhead"
    ),
    command!(
        "mark_out",
        "Trimming",
        MarkOut,
        Keys,
        ["o"],
        "End at the playhead"
    ),
    command!(
        "jump_to_in",
        "Trimming",
        JumpToIn,
        Keys,
        ["shift-i"],
        "Jump to the start"
    ),
    command!(
        "jump_to_out",
        "Trimming",
        JumpToOut,
        Keys,
        ["shift-o"],
        "Jump to the end"
    ),
    command!(
        "reset_trim",
        "Trimming",
        ResetTrim,
        Keys,
        ["x"],
        "Reset to the whole clip"
    ),
    command!(
        "add_segment",
        "Trimming",
        AddSegment,
        Keys,
        ["n"],
        "Add a segment at the playhead"
    ),
    command!(
        "split_at_playhead",
        "Trimming",
        SplitAtPlayhead,
        Keys,
        ["s"],
        "Split the segment at the playhead"
    ),
    command!(
        "toggle_loop",
        "Trimming",
        ToggleLoop,
        Keys,
        ["r"],
        "Loop the selection"
    ),
    command!(
        "select_previous_clip",
        "Clips",
        SelectPreviousClip,
        Keys,
        ["up"],
        "Previous clip"
    ),
    command!(
        "select_next_clip",
        "Clips",
        SelectNextClip,
        Keys,
        ["down"],
        "Next clip"
    ),
    command!(
        "export",
        "Clips",
        ExportClip,
        Window,
        ["ctrl-enter"],
        "Export"
    ),
    command!(
        "copy_last_export",
        "Clips",
        CopyLastExport,
        Window,
        ["ctrl-shift-c"],
        "Copy the last export to the clipboard"
    ),
    command!(
        "save_frame",
        "Clips",
        SaveFrame,
        Window,
        ["ctrl-shift-s"],
        "Save the frame as PNG"
    ),
    command!(
        "save_frame_jpeg",
        "Clips",
        SaveFrameJpeg,
        Window,
        ["ctrl-alt-s"],
        "Save the frame as JPEG"
    ),
    command!(
        "delete_clip",
        "Clips",
        DeleteClip,
        Keys,
        ["delete"],
        "Delete"
    ),
    command!(
        "undo",
        "Clips",
        Undo,
        Keys,
        ["ctrl-z"],
        "Undo a trim, delete or archive move"
    ),
    command!(
        "redo",
        "Clips",
        Redo,
        Keys,
        ["ctrl-y", "ctrl-shift-z"],
        "Redo"
    ),
    command!(
        "open_folder",
        "Clips",
        OpenFolder,
        Window,
        ["ctrl-o"],
        "Open a folder"
    ),
    command!(
        "reload",
        "Clips",
        Reload,
        Window,
        ["f5", "ctrl-r"],
        "Rescan the folder"
    ),
    command!(
        "toggle_theme",
        "Clips",
        ToggleTheme,
        Window,
        ["ctrl-t"],
        "Light or dark theme"
    ),
    command!(
        "show_settings",
        "Clips",
        ShowSettings,
        Window,
        ["ctrl-,"],
        "Settings"
    ),
    command!(
        "show_shortcuts",
        "Clips",
        ShowShortcuts,
        Keys,
        ["?"],
        "Keyboard shortcuts"
    ),
    command!("quit", "Clips", Quit, App, ["ctrl-q"], "Quit"),
];

/// Bindings that are always there and cannot be changed: alternatives that
/// keep working while a text field has focus.
fn fixed_bindings() -> Vec<KeyBinding> {
    vec![
        KeyBinding::new("f11", ToggleFullscreen, Some(CONTEXT)),
        KeyBinding::new("f1", ShowShortcuts, Some(CONTEXT)),
        KeyBinding::new("ctrl-up", SelectPreviousClip, Some(CONTEXT)),
        KeyBinding::new("ctrl-down", SelectNextClip, Some(CONTEXT)),
    ]
}

/// Parses a keystroke sequence such as `ctrl-shift-s` or `g g`.
pub fn parse_keys(source: &str) -> Option<Vec<Keystroke>> {
    #[cfg(target_os = "macos")]
    const OS: OperatingSystem = OperatingSystem::Mac;
    #[cfg(not(target_os = "macos"))]
    const OS: OperatingSystem = OperatingSystem::Windows;
    let keystrokes: Option<Vec<_>> = source
        .split_whitespace()
        .map(|part| Keystroke::parse_with_os(part, OS).ok())
        .collect();
    keystrokes.filter(|keystrokes| !keystrokes.is_empty())
}

/// Normalises a list of keys typed by the user: trims each, drops empty
/// ones and rejects any that do not parse.
pub fn clean_keys(keys: &[String]) -> Result<Vec<String>, String> {
    keys.iter()
        .map(|key| key.trim().to_owned())
        .filter(|key| !key.is_empty())
        .map(|key| {
            if parse_keys(&key).is_some() {
                Ok(key)
            } else {
                Err(key)
            }
        })
        .collect()
}

/// The keys bound to `command` once `overrides` are applied.
pub fn keys_for<'a>(
    command: &'a Command,
    overrides: &'a BTreeMap<String, Vec<String>>,
) -> Vec<String> {
    match overrides.get(command.id) {
        Some(keys) => clean_keys(keys).unwrap_or_default(),
        None => command.defaults.iter().map(|key| key.to_string()).collect(),
    }
}

/// The keys bound to each command with the current settings.
pub fn effective() -> Vec<(&'static Command, Vec<String>)> {
    let overrides = crate::settings::get().key_bindings;
    COMMANDS
        .iter()
        .map(|command| (command, keys_for(command, &overrides)))
        .collect()
}

/// The configurable bindings currently registered, so they can be switched
/// off when the settings change.
#[derive(Default)]
struct Bound(Rc<RefCell<Vec<BoundKeys>>>);

/// Keys and the context they were bound in.
type BoundKeys = (String, Option<String>);

impl Global for Bound {}

fn context_for(scope: Scope) -> Option<String> {
    match scope {
        Scope::Keys => Some(format!("{CONTEXT} && !Input")),
        Scope::Window => Some(CONTEXT.to_owned()),
        Scope::App => None,
    }
}

fn load(
    keys: &str,
    action: Box<dyn Action>,
    context: Option<&str>,
    cx: &App,
) -> Option<KeyBinding> {
    let predicate = context
        .map(|context| KeyBindingContextPredicate::parse(context).ok().map(Rc::new))
        .unwrap_or(None);
    if context.is_some() && predicate.is_none() {
        return None;
    }
    KeyBinding::load(keys, action, predicate, false, None, cx).ok()
}

/// Registers the key bindings from the settings. Called at start-up and
/// again whenever the settings are saved: the previous configurable
/// bindings are switched off and the new ones take their place.
pub fn apply(cx: &mut App) {
    if !cx.has_global::<Bound>() {
        cx.set_global(Bound::default());
        cx.bind_keys(fixed_bindings());
    }
    let bound = cx.global::<Bound>().0.clone();
    let mut bindings = Vec::new();
    for (keys, context) in bound.borrow_mut().drain(..) {
        bindings.extend(load(&keys, Box::new(NoAction), context.as_deref(), cx));
    }
    for (command, keys) in effective() {
        let context = context_for(command.scope);
        for keys in keys {
            if let Some(binding) = load(&keys, (command.action)(), context.as_deref(), cx) {
                bindings.push(binding);
                bound.borrow_mut().push((keys, context.clone()));
            }
        }
    }
    cx.bind_keys(bindings);
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{COMMANDS, clean_keys, keys_for, parse_keys};

    #[test]
    fn default_keys_parse_and_ids_are_unique() {
        for command in COMMANDS {
            for key in command.defaults {
                assert!(parse_keys(key).is_some(), "{key}");
            }
            assert_eq!(
                COMMANDS
                    .iter()
                    .filter(|other| other.id == command.id)
                    .count(),
                1,
                "{}",
                command.id
            );
        }
    }

    #[test]
    fn overrides_replace_defaults() {
        let play = COMMANDS
            .iter()
            .find(|command| command.id == "toggle_play")
            .unwrap();
        let mut overrides = BTreeMap::new();
        assert_eq!(keys_for(play, &overrides), ["space", "k"]);
        overrides.insert("toggle_play".to_owned(), vec!["P".to_owned()]);
        assert_eq!(keys_for(play, &overrides), ["P"]);
        overrides.insert("toggle_play".to_owned(), Vec::new());
        assert!(keys_for(play, &overrides).is_empty());
    }

    #[test]
    fn rejects_keys_that_do_not_parse() {
        assert_eq!(
            clean_keys(&[" ctrl-k ".into(), "".into()]),
            Ok(vec!["ctrl-k".to_owned()])
        );
        assert!(clean_keys(&["foo-k".into()]).is_err());
    }
}
