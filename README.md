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
encode the exports. Exports are H.264, HEVC or AV1 at a Small, Balanced
or High quality preset. Each codec uses the first hardware encoder that
works (AMF, NVENC or Quick Sync) and falls back to `libx264`, `libx265`
or `libsvtav1`/`libaom-av1`.

## Usage

```sh
plic [FOLDER]
```

Without a folder argument, plic opens the current directory.

1. Pick a clip on the left. It starts in the viewer.
2. Trim it by dragging the yellow handles on the timeline, by pressing
   `I` and `O` at the playhead, or by typing the times in the export panel.
3. Name it, choose the destination folder, and press `Ctrl+Enter`.

To keep several parts of one recording, press `N` (or **Add segment at the
playhead**) to start another segment, and trim it like the first. Click a
segment on the timeline or in the export panel to edit it. Press `S` to
split the segment under the playhead in two, which cuts a long recording
into parts in a single pass. Segments are
joined in order into one file, or, with **Join into one file** off, saved
as `name-1`, `name-2`, … or under a name given to each part The original is only moved to `edited/` once every
part has exported.

Under **Frame**, crop the export to 16:9, 9:16, 1:1 or 4:5 (the viewer
shows what is kept, and a slider moves the crop), rotate it in quarter
turns, or flip it. These always re-encode.

Under **Audio**, pick which audio tracks to keep. Recordings with separate
game and microphone tracks are mixed into one; untick everything to drop
the sound. A gain slider and **Normalize loudness** (to -14 LUFS) change the
exported level without touching the viewer's volume.

Cap the **Size** at 1080p, 720p or 480p (by the shorter side, so vertical
video works too) and the **Rate** at 60 or 30 fps; plic never upscales or
adds frames. Pick a **File** size (10, 25, 50 or 100 MB) to fit an upload limit: plic works
out the bitrate from the export's length and encodes in two passes with
the codec's software encoder, which lands just under the size.

Switch the format to **GIF** or **WebP** for short, silent loops: pick a
width and frame rate, and the panel estimates the size. GIFs get a
palette made from the clip itself.

Turn on **Lossless** in the export panel to copy the video instead of
re-encoding it: the export is near-instant and keeps the original quality,
but the cut starts on the keyframe at or before the chosen start. The panel
shows where it will really start.

Press `Ctrl+Shift+S` (or the camera button above the viewer) to save the
frame under the playhead as a full-resolution PNG, or `Ctrl+Alt+S` for a
JPEG. It lands in the clip's destination folder as `name-01m12s345.png`.

Finished exports stay listed under the queue: copy one to the clipboard
as a file (paste it straight into Discord or Explorer) or show it in its
folder. `Ctrl+Shift+C` copies the latest one.

Exports run in the background, one at a time, while plic moves on to the
next clip. Press `?` to see every shortcut.

## Development

```sh
cargo test
mise run dev    # debug build on ./clips
mise run run    # release build, copied into ./clips
```
