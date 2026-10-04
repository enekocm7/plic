use std::{
    fs::{self, OpenOptions},
    io,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        OnceLock,
        atomic::{AtomicU64, Ordering},
    },
};

use anyhow::{Context, Result, bail};

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);
static ENCODER: OnceLock<&'static Encoder> = OnceLock::new();

struct Encoder {
    args: &'static [&'static str],
}

// Hardware encoders first; software x264 is the fallback and always works.
const ENCODERS: [Encoder; 4] = [
    Encoder {
        args: &["-c:v", "h264_amf", "-quality", "balanced", "-rc", "cqp", "-qp_i", "20", "-qp_p", "20"],
    },
    Encoder {
        args: &["-c:v", "h264_nvenc", "-preset", "p4", "-rc", "vbr", "-cq", "20", "-b:v", "0"],
    },
    Encoder {
        args: &["-c:v", "h264_qsv", "-preset", "fast", "-global_quality", "20"],
    },
    Encoder {
        args: &["-c:v", "libx264", "-preset", "fast", "-crf", "20"],
    },
];

/// Picks the first encoder that can actually encode a test clip on this machine.
fn encoder() -> &'static Encoder {
    ENCODER.get_or_init(|| {
        ENCODERS
            .iter()
            .find(|candidate| works(candidate))
            .unwrap_or(&ENCODERS[ENCODERS.len() - 1])
    })
}

fn works(encoder: &Encoder) -> bool {
    Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i"])
        .arg("color=c=black:s=640x360:r=30:d=0.5,format=yuv420p")
        .args(encoder.args)
        .args(["-f", "null", "-"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

pub fn export(
    input: &Path,
    output: &Path,
    start: Option<&str>,
    duration: Option<&str>,
) -> Result<()> {
    if output.exists() {
        bail!("{} already exists", output.display());
    }
    let temporary = create_temp_path(output)?;

    let mut command = Command::new("ffmpeg");
    command.args(["-hide_banner", "-loglevel", "error", "-y"]);
    if let Some(start) = start {
        command.args(["-ss", start]);
    }
    command.args(["-hwaccel", "auto"]).arg("-i").arg(input);
    if let Some(duration) = duration {
        command.args(["-t", duration]);
    }
    let result = command
        .args(encoder().args)
        .args(["-c:a", "aac", "-b:a", "192k"])
        .arg(&temporary)
        .output();
    let result = match result {
        Ok(result) => result,
        Err(error) => {
            let _ = fs::remove_file(&temporary);
            return Err(error).context("could not start ffmpeg; check that it is installed and on PATH");
        }
    };

    if !result.status.success() {
        let _ = fs::remove_file(&temporary);
        let stderr = String::from_utf8_lossy(&result.stderr).trim().to_owned();
        if stderr.is_empty() {
            bail!("ffmpeg exited with status {}", result.status);
        }
        bail!("ffmpeg exited with status {}: {stderr}", result.status);
    }

    publish(&temporary, output)
}

fn create_temp_path(output: &Path) -> Result<PathBuf> {
    let parent = output.parent().unwrap_or_else(|| Path::new("."));
    let stem = output
        .file_stem()
        .map(|stem| stem.to_string_lossy())
        .unwrap_or_default();
    for _ in 0..100 {
        let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let candidate = parent.join(format!(
            ".{stem}.plic-{}-{id}.mp4",
            std::process::id()
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(_) => return Ok(candidate),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("could not create temporary output in {}", parent.display())
                });
            }
        }
    }
    bail!("could not reserve a temporary output name in {}", parent.display());
}

fn publish(temporary: &Path, output: &Path) -> Result<()> {
    match fs::hard_link(temporary, output) {
        Ok(()) => {
            let _ = fs::remove_file(temporary);
            return Ok(());
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            let _ = fs::remove_file(temporary);
            bail!("{} already exists; choose another name", output.display());
        }
        Err(_) => {}
    }

    let mut destination = match OpenOptions::new().write(true).create_new(true).open(output) {
        Ok(file) => file,
        Err(error) => {
            let _ = fs::remove_file(temporary);
            if error.kind() == io::ErrorKind::AlreadyExists {
                bail!("{} already exists; choose another name", output.display());
            }
            return Err(error).with_context(|| format!("could not create {}", output.display()));
        }
    };

    let copy_result = (|| -> Result<()> {
        let mut source = fs::File::open(temporary)
            .with_context(|| format!("could not open {}", temporary.display()))?;
        io::copy(&mut source, &mut destination)
            .with_context(|| format!("could not copy video to {}", output.display()))?;
        destination
            .sync_all()
            .with_context(|| format!("could not finish writing {}", output.display()))?;
        Ok(())
    })();
    drop(destination);

    if let Err(error) = copy_result {
        let _ = fs::remove_file(output);
        let _ = fs::remove_file(temporary);
        return Err(error);
    }
    let _ = fs::remove_file(temporary);
    Ok(())
}
