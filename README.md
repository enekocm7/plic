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

Without a folder argument, plic reopens the folder from last time, or the
current directory the first time. The arrow next to **Open folder** lists
recent folders. The window size and position, volume and theme are
remembered too, in `state.json` next to `settings.json`.

You can also drop a folder onto the window to open it, or a video file to open
its folder with that clip selected. Dropped files are never moved.

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

Hovering the timeline shows a small preview of the frame under the
pointer. Filmstrips of the last few clips are kept, so flipping back to
one is instant.

The timeline shows the clip's audio waveform (every track mixed) along the
bottom of the filmstrip, so a shout or a shot is easy to find by sound.

Exports run in the background while plic moves on to the next clip. The
queue's "At once" buttons set how many may encode together (two by
default); hardware encoders are held to two sessions and software encoders
to one per four CPU cores, and lossless copies always run alongside. Press `?` to see every shortcut.

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

## Settings

Press `Ctrl+,` (or the gear in the title bar) to change the seek step, the
name of the folder originals move to after export (`edited` by default) and
the default destination folder. They are saved as `settings.json` in the
`plic` folder of your OS config directory (`%APPDATA%` on Windows) and can
also be edited there by hand.
