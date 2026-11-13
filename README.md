<div align="center">

<img src="assets/plic.png" alt="plic" width="96">

# plic

**Watch, trim and file away video clips.**

Open a folder of recordings, cut each one down, export it, and move on.
Finished originals step aside, so the list only shows what's left to do.

[Download](https://github.com/enekocm7/plic/releases/latest) ·
[Install](#install) ·
[How it works](#how-it-works) ·
[Settings](#settings)

</div>

---

## Highlights

- **Fast triage.** Play clips with sound, trim with handles or `I`/`O`, and
  export with `Ctrl+Enter`. Originals move to `edited/` once exported.
- **Multiple segments.** Keep several parts of one recording, or split it
  with `S`. Join the parts into one file or save them separately.
- **Batch actions.** Select many clips and export, move or delete them together.
- **Export your way.** H.264, HEVC or AV1, plus GIF and WebP. Crop, rotate,
  flip, downscale, cap the frame rate, or target a file size (10-100 MB).
- **Audio control.** Choose tracks, mix game and mic, adjust gain, normalize loudness.
- **Lossless cuts.** Copy the video without re-encoding for near-instant exports.
- **Background queue.** Keep working while exports run.
- **Safe to quit.** Trims and names are saved as drafts and restored next time.
- **Self-updating.** One click installs new versions.

## Install

**Windows:** download `plic-<version>-setup.exe` from the
[latest release](https://github.com/enekocm7/plic/releases/latest), or use Scoop:

```sh
scoop bucket add plic https://github.com/enekocm7/plic
scoop install plic/plic
```

The installer can add **Open with plic** to the Explorer menu for folders and
`.mp4`, `.mov`, `.mkv`, `.webm` and `.m4v` files. A portable `.zip` is on every
release too.

plic needs `ffmpeg`, and on Windows it fetches a verified build on first run,
so there's nothing to set up. See [ffmpeg](#ffmpeg) for details.

## How it works

```sh
plic [FOLDER]
```

With no folder, plic reopens the last one. You can also drop a folder or a
video onto the window.

1. **Pick** a clip on the left.
2. **Trim** it: drag the yellow handles, press `I` / `O` at the playhead, or type times.
3. **Export**: name it, choose a destination, press `Ctrl+Enter`.

Press `?` at any time to see every shortcut.

### Trimming

| Key | Action |
| --- | --- |
| `Space` | Play / pause |
| `I` / `O` | Set trim start / end at the playhead |
| `N` | Add another segment |
| `S` | Split the segment at the playhead |
| `J` `K` `L` | Shuttle backward / stop / forward (press again to go faster) |
| `=` `-` `0` | Zoom the timeline in / out / to fit |
| `Ctrl+Z` / `Ctrl+Y` | Undo / redo trims, moves and deletes |
| `Ctrl+Shift+S` | Save the current frame as PNG (`Ctrl+Alt+S` for JPEG) |
| `Ctrl+Shift+C` | Copy the latest export to the clipboard |

Zoom with the mouse wheel for frame-accurate cuts. Hovering the timeline
previews the frame, and the audio waveform makes a shout or a shot easy to find.

### Exporting

- **Format:** H.264, HEVC or AV1 at Small, Balanced or High quality. Hardware
  encoders (AMF, NVENC, Quick Sync) are used when available, with software fallbacks.
- **Frame:** crop to 16:9, 9:16, 1:1 or 4:5, rotate, or flip.
- **Size:** cap at 1080p, 720p or 480p and 60 or 30 fps. plic never upscales.
- **File size:** pick 10, 25, 50 or 100 MB and plic encodes in two passes to land just under it.
- **GIF / WebP:** short silent loops, with an estimated size.
- **Lossless:** copies the video as is. The cut snaps to the keyframe at or before your start, and the panel shows where.
- **Sections:** fold Frame, Audio or Quality to keep the panel short. Folded headers list any changed settings with a dot, and the choice is remembered next time.
- **Queue:** exports run in the background; the "At once" buttons set how many run together.

### Batch

`Ctrl`-click or `Shift`-click to select several clips, or press `Ctrl+A`.
A bar appears to export, move or delete them all. `Escape` goes back to a single clip.

### Undo and recovery

Deleted clips go to the Recycle Bin. Drafts (segments, names, destinations)
are kept in `drafts.json` in plic's data folder, never next to your recordings,
and are dropped once a clip is exported.

## Settings

Press `Ctrl+,` or click the gear. You can change the seek step, the folder
originals move to (`edited` by default), and the default destination.
They're stored in `settings.json` under `%APPDATA%\plic`.

**Name template** pre-fills export names, for example `{folder}-{date}-{n}`:

| Token | Meaning |
| --- | --- |
| `{name}` | The clip's own name |
| `{folder}` | The clips folder |
| `{date}` | The day it was recorded |
| `{n}` | A counter that skips names already in the destination |

**Key bindings** can be changed in the same dialog. In `settings.json` only
your changes are stored, and an empty list unbinds a command:

```json
"key_bindings": {
  "toggle_play": ["space", "p"],
  "delete_clip": []
}
```

### ffmpeg

On Windows, plic downloads ffmpeg 9.0.2 (Gyan Doshi's essentials build) into
`%LOCALAPPDATA%\plic\ffmpeg` on first run and checks it against a pinned
SHA-256. To use your own, set **ffmpeg folder** in the settings to a folder
containing `ffmpeg` and `ffprobe`. On other systems, plic uses what's on `PATH`.

## Updates

plic checks for a new release on start, at most every six hours. Click
**Install update** in the banner: it downloads the installer, verifies its
checksum, installs, and restarts. Portable copies open the release page
instead. Dismiss a version to hide it until a newer one appears; the download
button in the title bar checks manually.

## Development

Built with Rust and [GPUI](https://www.gpui.rs) via gpui-kit.

```sh
cargo test
mise run dev    # debug build on ./clips
mise run run    # release build, copied into ./clips
```

CI runs `cargo fmt --check`, `cargo clippy`, `cargo build` and `cargo test`
on Windows for every push to `master` and every pull request.

### Releasing

`Cargo.toml` holds the version, and the release fails if the tag doesn't match.

1. Bump `version` in `Cargo.toml` and run `cargo build` to update `Cargo.lock`.
2. Commit, then `git tag v0.9.0 && git push origin master v0.9.0`.

GitHub Actions publishes the installer, the portable `.zip`, the Scoop
manifest (rendered from `packaging/` by `render-manifests.ps1`) and
`SHA256SUMS.txt`, then commits the updated `bucket/plic.json` to `master`.

To build the installer locally, install [Inno Setup](https://jrsoftware.org/isinfo.php) and run:

```sh
cargo build --release
iscc /DAppVersion=0.9.1 installer\plic.iss
```

The result lands in `target/dist/`.

## License

[GPL-3.0](LICENSE)
