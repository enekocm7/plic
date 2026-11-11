//! The GPUI front end.

mod app;
mod frame_image;
mod library_panel;
mod player;
mod timeline;

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
        OpenFolder,
        Reload,
        ToggleTheme,
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
            // Video reads best on a dark surround, whatever the OS theme.
            Theme::change(ThemeMode::Dark, None, cx);
            bind_keys(cx);
            cx.on_action(|_: &Quit, cx| cx.quit());

            let options = WindowOptions {
                window_bounds: Some(WindowBounds::centered(size(px(1360.), px(860.)), cx)),
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
