use std::{
    fs::{self, OpenOptions},
    io,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

use anyhow::{Context, Result, bail};

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

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
    command.arg("-i").arg(input);
    if let Some(duration) = duration {
        command.args(["-t", duration]);
    }
    let result = command
        .args(["-c:v", "libx264", "-preset", "fast", "-crf", "20"])
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
