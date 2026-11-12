//! Where the `ffmpeg` and `ffprobe` programs come from.
//!
//! On Windows plic downloads a known ffmpeg build into its data directory
//! on first run, checks it against a pinned SHA-256 and uses it from then
//! on, so nobody has to touch `PATH`. A folder set in the settings takes
//! precedence; until the download has finished, or where there is no
//! managed build, the programs are looked up on `PATH`.

use std::{
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, bail};
use sha2::{Digest, Sha256};

/// One of the two programs plic runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Ffmpeg,
    Ffprobe,
}

impl Tool {
    fn name(self) -> &'static str {
        match self {
            Self::Ffmpeg => "ffmpeg",
            Self::Ffprobe => "ffprobe",
        }
    }

    fn file_name(self) -> String {
        format!("{}{}", self.name(), std::env::consts::EXE_SUFFIX)
    }
}

/// The ffmpeg build plic downloads, pinned by its checksum.
pub struct Build {
    pub version: &'static str,
    pub url: &'static str,
    pub sha256: &'static str,
    pub size: u64,
}

/// Gyan Doshi's "essentials" Windows build, from its GitHub releases.
#[cfg(windows)]
pub const BUILD: Option<Build> = Some(Build {
    version: "9.0.2",
    url: "https://github.com/GyanD/codexffmpeg/releases/download/9.0.2/ffmpeg-9.0.2-essentials_build.zip",
    sha256: "60f467265b1e312373dbcd92200c2618a74850f98d3d078e94296bb3fa2047ba",
    size: 114_768_076,
});

/// There is no managed build here; ffmpeg comes from the system.
#[cfg(not(windows))]
pub const BUILD: Option<Build> = None;

/// The folder the managed build is unpacked into.
fn managed_dir() -> Option<PathBuf> {
    let build = BUILD.as_ref()?;
    dirs::data_local_dir().map(|dir| dir.join("plic").join("ffmpeg").join(build.version))
}

/// The folder the user pointed the settings at, if any.
fn override_dir() -> Option<PathBuf> {
    let folder = crate::settings::get().ffmpeg_folder;
    let folder = folder.trim();
    (!folder.is_empty()).then(|| {
        let path = PathBuf::from(folder);
        // Pointing at the program itself works too.
        if path.is_file() {
            path.parent().map(Path::to_path_buf).unwrap_or(path)
        } else {
            path
        }
    })
}

/// The program to run for `tool`.
pub fn program(tool: Tool) -> PathBuf {
    [override_dir(), managed_dir()]
        .into_iter()
        .flatten()
        .map(|dir| dir.join(tool.file_name()))
        .find(|path| path.is_file())
        .unwrap_or_else(|| PathBuf::from(tool.name()))
}

/// Whether the managed build should be downloaded: there is one for this
/// system, it is not there yet, and no folder of the user's replaces it.
pub fn needs_download() -> bool {
    if override_dir().is_some_and(|dir| dir.join(Tool::Ffmpeg.file_name()).is_file()) {
        return false;
    }
    managed_dir().is_some_and(|dir| {
        ![Tool::Ffmpeg, Tool::Ffprobe]
            .iter()
            .all(|tool| dir.join(tool.file_name()).is_file())
    })
}

/// Downloads the managed build, checks its checksum and unpacks the two
/// programs. `progress` gets the fraction downloaded so far.
pub fn download(progress: impl Fn(f32)) -> Result<()> {
    let build = BUILD
        .as_ref()
        .context("there is no ffmpeg build to download here")?;
    let target = managed_dir().context("there is no data directory")?;
    let parent = target.parent().context("data directory has no parent")?;
    fs::create_dir_all(parent).with_context(|| format!("could not create {}", parent.display()))?;

    let archive_path = parent.join(format!("ffmpeg-{}.zip.part", build.version));
    let result =
        fetch(build, &archive_path, &progress).and_then(|()| unpack(&archive_path, &target));
    fs::remove_file(&archive_path).ok();
    result
}

/// Saves the archive at `path`, failing unless its SHA-256 matches.
fn fetch(build: &Build, path: &Path, progress: &impl Fn(f32)) -> Result<()> {
    let response = ureq::get(build.url)
        .header("User-Agent", concat!("plic/", env!("CARGO_PKG_VERSION")))
        .call()
        .context("could not reach the download server")?;
    let total = response
        .body()
        .content_length()
        .unwrap_or(build.size)
        .max(1);
    let mut body = response.into_body().into_reader();
    let mut file =
        fs::File::create(path).with_context(|| format!("could not create {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 256 * 1024];
    let mut received = 0u64;
    loop {
        let read = match body.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error).context("the download was interrupted"),
        };
        hasher.update(&buffer[..read]);
        file.write_all(&buffer[..read])
            .context("could not save the download")?;
        received += read as u64;
        progress((received as f64 / total as f64).min(1.0) as f32);
    }
    file.flush().context("could not save the download")?;
    let digest = hex(&hasher.finalize());
    if digest != build.sha256 {
        bail!(
            "the download is damaged or was changed (SHA-256 {digest}, expected {})",
            build.sha256
        );
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Takes `ffmpeg` and `ffprobe` out of the archive's `bin` folder into
/// `target`, which only appears once both are there.
fn unpack(archive: &Path, target: &Path) -> Result<()> {
    let file = fs::File::open(archive).context("could not open the download")?;
    let mut zip = zip::ZipArchive::new(file).context("the download is not a zip archive")?;
    let staging = target.with_extension("unpacking");
    if staging.exists() {
        fs::remove_dir_all(&staging).ok();
    }
    fs::create_dir_all(&staging)
        .with_context(|| format!("could not create {}", staging.display()))?;
    for tool in [Tool::Ffmpeg, Tool::Ffprobe] {
        let wanted = format!("bin/{}", tool.file_name());
        let index = (0..zip.len())
            .find(|&index| {
                zip.name_for_index(index)
                    .and_then(Result::ok)
                    .is_some_and(|name| name.replace('\\', "/").ends_with(&wanted))
            })
            .with_context(|| format!("the download has no {}", tool.file_name()))?;
        let mut entry = zip.by_index(index)?;
        let destination = staging.join(tool.file_name());
        let mut out = fs::File::create(&destination)
            .with_context(|| format!("could not create {}", destination.display()))?;
        io::copy(&mut entry, &mut out)
            .with_context(|| format!("could not unpack {}", tool.file_name()))?;
    }
    if target.exists() {
        fs::remove_dir_all(target).ok();
    }
    fs::rename(&staging, target)
        .with_context(|| format!("could not move ffmpeg into {}", target.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_print_as_lowercase_hex() {
        assert_eq!(
            hex(&Sha256::digest(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn unpacks_the_programs_from_the_bin_folder() -> Result<()> {
        let root = std::env::temp_dir().join(format!("plic-unpack-{}", std::process::id()));
        fs::create_dir_all(&root)?;
        let archive = root.join("build.zip");
        {
            let mut writer = zip::ZipWriter::new(fs::File::create(&archive)?);
            let options = zip::write::SimpleFileOptions::default();
            for tool in [Tool::Ffmpeg, Tool::Ffprobe] {
                writer.start_file(format!("ffmpeg-x/bin/{}", tool.file_name()), options)?;
                writer.write_all(tool.name().as_bytes())?;
            }
            writer.start_file("ffmpeg-x/README.txt", options)?;
            writer.finish()?;
        }
        let target = root.join("9.9");
        unpack(&archive, &target)?;
        assert_eq!(
            fs::read(target.join(Tool::Ffprobe.file_name()))?,
            b"ffprobe"
        );
        assert!(!target.with_extension("unpacking").exists());
        fs::remove_dir_all(&root)?;
        Ok(())
    }

    /// Downloads the real build (about 110 MB), so it only runs when asked.
    #[test]
    #[ignore]
    #[cfg(windows)]
    fn downloads_and_verifies_the_pinned_build() -> Result<()> {
        let build = BUILD.as_ref().unwrap();
        let root = std::env::temp_dir().join(format!("plic-fetch-{}", std::process::id()));
        fs::create_dir_all(&root)?;
        let archive = root.join("build.zip");
        fetch(build, &archive, &|_| {})?;
        unpack(&archive, &root.join("bin"))?;
        assert!(root.join("bin").join("ffmpeg.exe").is_file());
        fs::remove_dir_all(&root)?;
        Ok(())
    }
}
