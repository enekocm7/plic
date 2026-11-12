# plic

Watch, trim and file away video clips.

plic opens a folder of recordings, plays them back with sound, and lets
you cut each one down and save it into a subfolder or beside the originals. Once a clip is
exported, its original moves to `edited/`, so the list only holds what is
still to be done.

Built with [GPUI](https://www.gpui.rs) through gpui-kit and its
`gpui-fast` backend.

## Requirements

plic decodes the preview and encodes the exports with `ffmpeg` and
`ffprobe`, and looks after them itself. On Windows, the first run downloads
a known build (ffmpeg 9.0.2, Gyan Doshi's essentials build) into
`%LOCALAPPDATA%\plic\ffmpeg`, checks it against a pinned SHA-256 and uses
it from then on; a banner shows the progress and offers a retry if the
download fails. Nothing has to be put on `PATH`. To use your own build, set
**ffmpeg folder** in the settings to the folder holding `ffmpeg` and
`ffprobe`. On other systems, and until the download has finished, plic uses
the ones on `PATH`.

Exports are H.264, HEVC or AV1 at a Small, Balanced
or High quality preset. Each codec uses the first hardware encoder that
works (AMF, NVENC or Quick Sync) and falls back to `libx264`, `libx265`
or `libsvtav1`/`libaom-av1`.

## Install

Download `plic-<version>-setup.exe` from the
[latest release](https://github.com/enekocm7/plic/releases/latest), or use a
package manager:

```sh
winget install enekocm7.plic
scoop bucket add plic https://github.com/enekocm7/plic
scoop install plic/plic
```

The installer (and the Scoop package) add **Open with plic** to the Explorer
menu of folders, of the empty space inside a folder, and of `.mp4`, `.mov`,
`.mkv`, `.webm` and `.m4v` files; opening a video opens its folder with that
clip selected. The installer lets you leave this out, and uninstalling
removes it. A portable `.zip` is on every release too.

## Usage

```sh
plic [FOLDER]
```

Without a folder argument, plic reopens the folder from last time, or the
current directory the first time. The arrow next to **Open folder** lists
recent folders. The window size and position, volume and theme are
remembered too, in `state.json` next to `settings.json`.

Unfinished work survives a restart: each clip's segments, export name and
destination are kept in `drafts.json` in plic's data folder (`%APPDATA%\plic`
on Windows), never next to the recordings, and are dropped once the clip
has been exported. A recording replaced by a different file starts fresh.

You can also drop a folder onto the window to open it, or a video file to open
its folder with that clip selected. Dropped files are never moved.

1. Pick a clip on the left. It starts in the viewer.
2. Trim it by dragging the yellow handles on the timeline, by pressing
   `I` and `O` at the playhead, or by typing the times in the export panel.
3. Name it, choose the destination folder, and press `Ctrl+Enter`.

Ctrl-click clips to pick several, Shift-click to pick a run of them, or
press `Ctrl+A` for every clip the list shows. A bar over the list then
exports them all (`Ctrl+Enter`) with the export panel's format and the
selected clip's destination, naming each with the name template or its own
name; moves them into another folder; or deletes them (`Delete`). `Escape`
goes back to a single clip.

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

`Ctrl+Z` undoes the last trim change (a whole drag counts as one), delete
or move of an original into `edited/`; `Ctrl+Y` or `Ctrl+Shift+Z` redoes
it. Deleted clips come back out of the Recycle Bin. The arrows in the title
bar do the same and say what they will undo.

Finished exports stay listed under the queue: copy one to the clipboard
as a file (paste it straight into Discord or Explorer) or show it in its
folder. `Ctrl+Shift+C` copies the latest one.

Zoom the timeline with the mouse wheel (around the pointer) or `=` and `-`
(around the playhead) to trim long recordings precisely; `0` shows the
whole clip again. Shift with the wheel scrolls along it, the view follows
the playhead, and once frames are far enough apart each one gets a tick.
The speed button next to the loop toggle plays at 0.5x, 1x, 1.5x or 2x;
the sound keeps its pitch. `J`, `K` and `L` work as a shuttle: `L` plays
forward and each further press goes faster, `J` plays backward the same
way, and `K` stops. The arrow keys still jump by the seek step.

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
(`plic-<version>-setup.exe`), a portable `.zip`, the package manager
manifests and `SHA256SUMS.txt`. The manifests are filled in from the
templates in `packaging/` by `packaging/render-manifests.ps1`. Then:

- **Scoop**: `bucket/plic.json` on `master` is updated to the new release,
  so `scoop update` picks it up.
- **winget**: with a `WINGET_TOKEN` secret (a classic GitHub token with the
  `public_repo` scope), `wingetcreate` opens a pull request on
  `microsoft/winget-pkgs`. Without it, submit the manifests attached to the
  release by hand.

plic checks for a newer release when it starts, at most once every six
hours, and shows a banner. "Install update" downloads the installer, checks
it against `SHA256SUMS.txt`, runs it silently and restarts plic on the new
version. A portable copy opens the release page instead. Dismissing a version
hides it until an even newer one is published; the download button in the
title bar checks again by hand.

To build the installer locally, install [Inno Setup](https://jrsoftware.org/isinfo.php)
and run:

```sh
cargo build --release
iscc /DAppVersion=0.8.1 installer\plic.iss
```

The installer lands in `target/dist/`.

## Settings

Press `Ctrl+,` (or the gear in the title bar) to change the seek step, the
name of the folder originals move to after export (`edited` by default) and
the default destination folder. They are saved as `settings.json` in the
`plic` folder of your OS config directory (`%APPDATA%` on Windows) and can
also be edited there by hand.

A **name template** in the settings pre-fills export names, e.g.
`{folder}-{date}-{n}`. `{name}` is the clip's own name, `{folder}` the clips
folder, `{date}` the day it was recorded, and `{n}` counts up past names that
already exist in the destination.

Every keyboard shortcut can be changed under **Key bindings** in the same
dialog. In `settings.json` only the changed ones are stored, by command name,
each with a list of keys; an empty list unbinds a command:

```json
"key_bindings": {
  "toggle_play": ["space", "p"],
  "delete_clip": []
}
```

The shortcuts reference (`?`) always shows the keys in effect.

## License

plic is free software under the GNU General Public License, version 3 —
see [LICENSE](LICENSE).
