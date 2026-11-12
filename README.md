# plic

Watch, trim and file away video clips.

plic opens a folder of recordings, plays them back with sound, and lets
you cut each one down and save it into a subfolder. Once a clip is
exported, its original moves to `edited/`, so the list only holds what is
still to be done.

Built with [GPUI](https://www.gpui.rs) through gpui-kit and its
`gpui-fast` backend.

## Requirements

`ffmpeg` and `ffprobe` must be on `PATH`. They decode the preview and
encode the exports. Exports use the first hardware H.264 encoder that
works (AMF, NVENC or Quick Sync) and fall back to `libx264`.

## Usage

```sh
plic [FOLDER]
```

Without a folder argument, plic opens the current directory.

1. Pick a clip on the left. It starts in the viewer.
2. Trim it by dragging the yellow handles on the timeline, by pressing
   `I` and `O` at the playhead, or by typing the times in the export panel.
3. Name it, choose the destination folder, and press `Ctrl+Enter`.

Turn on **Lossless** in the export panel to copy the video instead of
re-encoding it: the export is near-instant and keeps the original quality,
but the cut starts on the keyframe at or before the chosen start. The panel
shows where it will really start.

Exports run in the background, one at a time, while plic moves on to the
next clip. Press `?` to see every shortcut.

## Development

```sh
cargo test
mise run dev    # debug build on ./clips
mise run run    # release build, copied into ./clips
```

CI runs `cargo fmt --check`, `cargo clippy`, `cargo build` and `cargo test`
on Windows for every push to `master` and every pull request.

## Releases

Releases are built by GitHub Actions from version tags. `Cargo.toml` is the
single source of the version; the release fails if the tag does not match it.

1. Bump `version` in `Cargo.toml` and run `cargo build` to update `Cargo.lock`.
2. Commit, then tag and push: `git tag v0.9.0 && git push origin master v0.9.0`.

The workflow publishes a GitHub release with an installer
(`plic-<version>-setup.exe`), a portable `.zip` and `SHA256SUMS.txt`.

To build the installer locally, install [Inno Setup](https://jrsoftware.org/isinfo.php)
and run:

```sh
cargo build --release
iscc /DAppVersion=0.8.0 installer\plic.iss
```

The installer lands in `target/dist/`.
