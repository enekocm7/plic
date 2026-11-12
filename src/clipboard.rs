//! Putting exported files on the system clipboard, so they can be pasted
//! straight into a chat or a file manager.

use std::path::Path;

use anyhow::Result;

/// Places `path` on the clipboard as a file, the way Explorer's Copy does
/// (the `CF_HDROP` file list format).
#[cfg(windows)]
pub fn copy_file(path: &Path) -> Result<()> {
    use anyhow::{Context as _, anyhow};

    let path = path
        .to_str()
        .context("the file name cannot be put on the clipboard")?;
    let _clipboard = clipboard_win::Clipboard::new_attempts(10)
        .map_err(|error| anyhow!("could not open the clipboard: {error}"))?;
    clipboard_win::empty().map_err(|error| anyhow!("could not clear the clipboard: {error}"))?;
    clipboard_win::raw::set_file_list(&[path])
        .map_err(|error| anyhow!("could not copy the file: {error}"))
}

#[cfg(not(windows))]
pub fn copy_file(_: &Path) -> Result<()> {
    anyhow::bail!("copying files to the clipboard is only supported on Windows")
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    /// Replaces whatever is on the clipboard: `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn puts_the_file_on_the_clipboard() {
        let path = std::env::temp_dir().join(format!("plic-clip-{}.mp4", std::process::id()));
        std::fs::write(&path, b"x").unwrap();
        copy_file(&path).unwrap();
        let files: Vec<String> =
            clipboard_win::get_clipboard(clipboard_win::formats::FileList).unwrap();
        std::fs::remove_file(&path).ok();
        assert_eq!(files, [path.to_string_lossy().into_owned()]);
    }
}
