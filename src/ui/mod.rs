//! The GPUI front end.

mod app;
mod clip_actions;
mod clip_filter;
mod editor;
mod frame_image;
mod jobs;
mod library_panel;
mod player;
mod settings_panel;
mod shortcuts;
mod timeline;
mod update;
mod volume;

use std::path::PathBuf;

use gpui_kit::component::{Theme, ThemeMode, TitleBar};
use gpui_kit::*;

pub use app::PlicApp;

/// Key context of the main window. Single-key shortcuts are bound with
/// `!Input` so they never fire while a text field has focus.
pub const CONTEXT: &str = "Plic";

actions!(
    plic,
    [
        SelectPreviousClip,
        SelectNextClip,
        TogglePlay,
        SeekBackward,
        SeekForward,
        StepBackward,
        StepForward,
        JumpToStart,
        JumpToEnd,
        ToggleMute,
        ToggleMarker,
        NextMarker,
        PreviousMarker,
        TrimToMarkers,
        ToggleFullscreen,
        ExitFullscreen,
        MarkIn,
        MarkOut,
        ResetTrim,
        AddSegment,
        SplitAtPlayhead,
        ToggleLoop,
        JumpToIn,
        JumpToOut,
        ExportClip,
        SaveFrame,
        CopyLastExport,
        SaveFrameJpeg,
        DeleteClip,
        OpenFolder,
        Reload,
        ToggleTheme,
        ShowShortcuts,
        ShowSettings,
        Quit,
    ]
);

fn bind_keys(cx: &mut App) {
    let shortcuts = format!("{CONTEXT} && !Input");
    let shortcuts = Some(shortcuts.as_str());
    cx.bind_keys([
        KeyBinding::new("up", SelectPreviousClip, shortcuts),
        KeyBinding::new("down", SelectNextClip, shortcuts),
        KeyBinding::new("space", TogglePlay, shortcuts),
        KeyBinding::new("k", TogglePlay, shortcuts),
        KeyBinding::new("left", SeekBackward, shortcuts),
        KeyBinding::new("right", SeekForward, shortcuts),
        KeyBinding::new("j", SeekBackward, shortcuts),
        KeyBinding::new("l", SeekForward, shortcuts),
        KeyBinding::new(",", StepBackward, shortcuts),
        KeyBinding::new(".", StepForward, shortcuts),
        KeyBinding::new("home", JumpToStart, shortcuts),
        KeyBinding::new("end", JumpToEnd, shortcuts),
        KeyBinding::new("m", ToggleMute, shortcuts),
        KeyBinding::new("b", ToggleMarker, shortcuts),
        KeyBinding::new("]", NextMarker, shortcuts),
        KeyBinding::new("[", PreviousMarker, shortcuts),
        KeyBinding::new("shift-b", TrimToMarkers, shortcuts),
        KeyBinding::new("f", ToggleFullscreen, shortcuts),
        KeyBinding::new("escape", ExitFullscreen, shortcuts),
        KeyBinding::new("i", MarkIn, shortcuts),
        KeyBinding::new("o", MarkOut, shortcuts),
        KeyBinding::new("shift-i", JumpToIn, shortcuts),
        KeyBinding::new("shift-o", JumpToOut, shortcuts),
        KeyBinding::new("x", ResetTrim, shortcuts),
        KeyBinding::new("n", AddSegment, shortcuts),
        KeyBinding::new("s", SplitAtPlayhead, shortcuts),
        KeyBinding::new("r", ToggleLoop, shortcuts),
        KeyBinding::new("delete", DeleteClip, shortcuts),
        KeyBinding::new("?", ShowShortcuts, shortcuts),
        KeyBinding::new("f11", ToggleFullscreen, Some(CONTEXT)),
        KeyBinding::new("f1", ShowShortcuts, Some(CONTEXT)),
        KeyBinding::new("ctrl-,", ShowSettings, Some(CONTEXT)),
        // Works from inside the editor's text fields too.
        KeyBinding::new("ctrl-enter", ExportClip, Some(CONTEXT)),
        KeyBinding::new("ctrl-shift-s", SaveFrame, Some(CONTEXT)),
        KeyBinding::new("ctrl-shift-c", CopyLastExport, Some(CONTEXT)),
        KeyBinding::new("ctrl-alt-s", SaveFrameJpeg, Some(CONTEXT)),
        KeyBinding::new("ctrl-up", SelectPreviousClip, Some(CONTEXT)),
        KeyBinding::new("ctrl-down", SelectNextClip, Some(CONTEXT)),
        KeyBinding::new("ctrl-o", OpenFolder, Some(CONTEXT)),
        KeyBinding::new("f5", Reload, Some(CONTEXT)),
        KeyBinding::new("ctrl-r", Reload, Some(CONTEXT)),
        KeyBinding::new("ctrl-t", ToggleTheme, Some(CONTEXT)),
        KeyBinding::new("ctrl-q", Quit, None),
    ]);
}

/// Opens the main window and runs until it closes.
pub fn run(folder: Option<PathBuf>) {
    gpui_kit::application()
        .with_assets(gpui_kit::assets::AllAssets)
        .run(move |cx| {
            gpui_kit::init(cx);
            // Probing the hardware encoders takes a moment; do it before the
            // first export needs it.
            crate::ffmpeg::warm_up_encoder(crate::ffmpeg::Codec::default());
            // Video reads best on a dark surround, whatever the OS theme.
            let mode = if crate::state::get().dark_theme {
                ThemeMode::Dark
            } else {
                ThemeMode::Light
            };
            Theme::change(mode, None, cx);
            bind_keys(cx);
            cx.on_action(|_: &Quit, cx| {
                crate::state::save();
                cx.quit()
            });
            cx.on_window_closed(|_, _| crate::state::save()).detach();

            let options = WindowOptions {
                window_bounds: Some(
                    remembered_bounds()
                        .unwrap_or_else(|| WindowBounds::centered(size(px(1360.), px(860.)), cx)),
                ),
                window_min_size: Some(size(px(960.), px(600.))),
                app_id: Some("plic".into()),
                ..TitleBar::window_options()
            };
            gpui_kit::open_window(options, cx, move |window, cx| {
                window.set_window_title("plic");
                cx.new(|cx| PlicApp::new(folder, window, cx))
            })
            .expect("failed to open the main window");
            cx.activate(true);
        });
}

/// The window bounds from the last run, if they are worth restoring.
fn remembered_bounds() -> Option<WindowBounds> {
    let rect = crate::state::get().window?;
    if rect.width < 960. || rect.height < 600. {
        return None;
    }
    let bounds = Bounds {
        origin: point(px(rect.x), px(rect.y)),
        size: size(px(rect.width), px(rect.height)),
    };
    Some(if rect.maximized {
        WindowBounds::Maximized(bounds)
    } else {
        WindowBounds::Windowed(bounds)
    })
}

/// Remembers where the window is, to restore it on the next run.
fn record_bounds(window: &Window) {
    let (bounds, maximized) = match window.window_bounds() {
        WindowBounds::Windowed(bounds) => (bounds, false),
        WindowBounds::Maximized(bounds) => (bounds, true),
        WindowBounds::Fullscreen(_) => return,
    };
    let rect = crate::state::WindowRect {
        x: f32::from(bounds.origin.x),
        y: f32::from(bounds.origin.y),
        width: f32::from(bounds.size.width),
        height: f32::from(bounds.size.height),
        maximized,
    };
    crate::state::update(|state| state.window = Some(rect));
}
