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

Exports run in the background, one at a time, while plic moves on to the
next clip. Press `?` to see every shortcut.

## Development

```sh
cargo test
mise run dev    # debug build on ./clips
mise run run    # release build, copied into ./clips
```
