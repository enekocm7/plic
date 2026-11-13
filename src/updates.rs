//! Checking GitHub for a newer release of plic.
//!
//! The check is one unauthenticated request to the releases API. The
//! version the user dismissed, and when the last check ran, live in the
//! state plic already keeps between runs.

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use sha2::{Digest, Sha256};

/// The running version. The release workflow refuses a tag that does not
/// match it, so this is also the version on the latest release page.
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Where the latest published release is asked for.
const LATEST_RELEASE_API: &str = "https://api.github.com/repos/enekocm7/plic/releases/latest";

/// The page the update banner links to.
pub const RELEASES_URL: &str = "https://github.com/enekocm7/plic/releases/latest";

/// How long an automatic check waits before asking GitHub again. Restarts
/// within this window reuse the previous answer.
pub const CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

/// A release newer than the running build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Update {
    /// The version without its leading `v`, e.g. `0.9.0`.
    pub version: String,
    /// The release page, which carries the notes and the downloads.
    pub url: String,
    /// The installer asset, when the release carries one.
    pub installer: Option<Download>,
    /// The `SHA256SUMS.txt` list, used when GitHub does not report the
    /// installer's own digest.
    pub checksums: Option<Download>,
}

/// A file attached to a release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Download {
    pub name: String,
    pub url: String,
    /// The lowercase hex SHA-256 GitHub computed for the file, when it
    /// reports one.
    pub sha256: Option<String>,
}

/// What the user has chosen about update checks, kept between runs.
pub fn dismissed() -> Option<String> {
    crate::state::get().dismissed_update
}

/// Keeps `version` from being offered again.
pub fn dismiss(version: &str) {
    let version = version.to_string();
    crate::state::update(|state| state.dismissed_update = Some(version));
    crate::state::save();
}

/// Offers a dismissed version again, as when the user checks by hand.
pub fn forget_dismissal() {
    crate::state::update(|state| state.dismissed_update = None);
    crate::state::save();
}

/// Whether enough time has passed for the automatic check to run again.
pub fn is_due() -> bool {
    is_due_at(crate::state::get().last_update_check, unix_seconds())
}

fn is_due_at(last_checked: Option<u64>, now: u64) -> bool {
    !last_checked.is_some_and(|at| now.saturating_sub(at) < CHECK_INTERVAL.as_secs())
}

/// Notes that a check succeeded, whatever it found, so the next automatic
/// one can wait.
pub fn mark_checked() {
    let at = unix_seconds();
    crate::state::update(|state| state.last_update_check = Some(at));
    crate::state::save();
}

/// Asks GitHub for the latest release, and returns it when it is newer than
/// the running version.
pub fn check() -> Result<Option<Update>> {
    let Some(release) = latest_release()? else {
        return Ok(None);
    };
    // `/releases/latest` already skips these, but a stray one must never be
    // offered as the newest stable version.
    if release.draft || release.prerelease {
        return Ok(None);
    }
    let version = release.tag_name.trim_start_matches(['v', 'V']);
    if !is_newer(version, CURRENT_VERSION) {
        return Ok(None);
    }
    Ok(Some(Update {
        version: version.to_string(),
        url: release.url(),
        installer: release.asset(|name| name.ends_with("-setup.exe")),
        checksums: release.asset(|name| name == "SHA256SUMS.txt"),
    }))
}

/// The part of GitHub's release JSON that the check needs.
#[derive(Debug, Deserialize)]
struct Release {
    tag_name: String,
    /// Present on a published release; older API answers may omit it.
    #[serde(default)]
    html_url: Option<String>,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<Asset>,
}

#[derive(Debug, Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
    /// `sha256:<hex>`, computed by GitHub when the asset was uploaded.
    #[serde(default)]
    digest: Option<String>,
}

impl Asset {
    fn sha256(&self) -> Option<String> {
        let hex = self.digest.as_deref()?.strip_prefix("sha256:")?;
        (hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .then(|| hex.to_ascii_lowercase())
    }
}

impl Release {
    fn asset(&self, matches: impl Fn(&str) -> bool) -> Option<Download> {
        self.assets
            .iter()
            .find(|asset| matches(&asset.name))
            .map(|asset| Download {
                name: asset.name.clone(),
                url: asset.browser_download_url.clone(),
                sha256: asset.sha256(),
            })
    }

    fn url(&self) -> String {
        self.html_url
            .clone()
            .unwrap_or_else(|| RELEASES_URL.to_string())
    }
}

/// The latest published release, or `None` while the repository has none.
fn latest_release() -> Result<Option<Release>> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        // A hung connection must not leave the check running forever.
        .timeout_global(Some(Duration::from_secs(10)))
        // GitHub turns down requests without a User-Agent.
        .user_agent(format!("plic/{CURRENT_VERSION}"))
        .build()
        .into();
    let mut response = match agent
        .get(LATEST_RELEASE_API)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .call()
    {
        Ok(response) => response,
        // GitHub answers 404 until the first release is published.
        Err(ureq::Error::StatusCode(404)) => return Ok(None),
        Err(error) => {
            return Err(error).context("could not ask GitHub for the latest release");
        }
    };
    let body = response
        .body_mut()
        .read_to_string()
        .context("could not read GitHub's response")?;
    let release: Release =
        serde_json::from_str(&body).context("GitHub returned an unexpected response")?;
    Ok(Some(release))
}

/// Whether `candidate` comes after `current`. Both look like `X.Y.Z`;
/// anything that does not parse is never newer, so a malformed tag cannot
/// nag the user.
fn is_newer(candidate: &str, current: &str) -> bool {
    match (parse_version(candidate), parse_version(current)) {
        (Some(candidate), Some(current)) => candidate > current,
        _ => false,
    }
}

/// The numeric components of a version, ignoring a `-prerelease` or
/// `+build` suffix.
fn parse_version(version: &str) -> Option<[u64; 3]> {
    let core = version.split(['-', '+']).next()?;
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some([major, minor, patch])
}

/// Whether this copy was put in place by the installer, which leaves its
/// uninstaller next to the executable. A portable copy has to be replaced
/// by hand, because running the installer would add a second copy.
pub fn is_installed() -> bool {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("unins000.exe")))
        .is_some_and(|uninstaller| uninstaller.is_file())
}

/// Downloads the installer for `update`, checks its SHA-256 against the
/// digest GitHub reports (or the release's `SHA256SUMS.txt`), and returns
/// where it was saved.
pub fn download(update: &Update) -> Result<PathBuf> {
    let Some(installer) = &update.installer else {
        bail!("this release has no installer to download");
    };
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(15)))
        .user_agent(format!("plic/{CURRENT_VERSION}"))
        .build()
        .into();

    let expected = match (&installer.sha256, &update.checksums) {
        (Some(digest), _) => digest.clone(),
        (None, Some(checksums)) => {
            let sums = agent
                .get(&checksums.url)
                .call()
                .context("could not download the checksum list")?
                .body_mut()
                .read_to_string()
                .context("could not read the checksum list")?;
            expected_checksum(&sums, &installer.name)
                .with_context(|| format!("no checksum is published for {}", installer.name))?
        }
        (None, None) => bail!("no checksum is published for {}", installer.name),
    };

    let dir = std::env::temp_dir().join(format!("plic-update-{}", update.version));
    fs::create_dir_all(&dir).context("could not create a folder for the download")?;
    let path = dir.join(&installer.name);

    let response = agent
        .get(&installer.url)
        .call()
        .context("could not download the installer")?;
    let mut body = response.into_body().into_reader();
    let mut file = fs::File::create(&path).context("could not save the installer")?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 256 * 1024];
    loop {
        let read = match body.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error).context("the download was interrupted"),
        };
        hasher.update(&buffer[..read]);
        file.write_all(&buffer[..read])
            .context("could not save the installer")?;
    }
    file.flush().context("could not save the installer")?;
    drop(file);

    let digest: String = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    if digest != expected {
        fs::remove_file(&path).ok();
        bail!("the download is damaged or was changed (SHA-256 {digest}, expected {expected})");
    }
    Ok(path)
}

/// The hash listed for `name` in a `sha256sum`-style file.
fn expected_checksum(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (hash, file) = line.split_once(char::is_whitespace)?;
        (file.trim().trim_start_matches('*') == name).then(|| hash.to_ascii_lowercase())
    })
}

/// Starts the downloaded installer quietly. It closes plic if it is still
/// running, replaces it in place and starts it again, so the caller should
/// quit right after this returns.
pub fn launch_installer(path: &Path) -> Result<()> {
    let mut command = std::process::Command::new(path);
    command.args([
        "/SILENT",
        "/SUPPRESSMSGBOXES",
        "/NORESTART",
        "/CLOSEAPPLICATIONS",
        "/RELAUNCH=1",
    ]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP: outlive plic.
        command.creation_flags(0x0000_0008 | 0x0000_0200);
    }
    command.spawn().context("could not start the installer")?;
    Ok(())
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions_component_by_component() {
        assert!(is_newer("0.9.0", "0.8.0"));
        assert!(is_newer("0.10.0", "0.9.9"));
        assert!(is_newer("1.0.0", "0.99.99"));
        assert!(!is_newer("0.8.0", "0.8.0"));
        assert!(!is_newer("0.8.0", "0.9.0"));
    }

    #[test]
    fn malformed_versions_are_never_newer() {
        assert!(!is_newer("latest", "0.8.0"));
        assert!(!is_newer("0.9", "0.8.0"));
        assert!(!is_newer("0.9.0.1", "0.8.0"));
        assert!(!is_newer("", "0.8.0"));
    }

    #[test]
    fn prerelease_suffixes_are_ignored() {
        assert_eq!(parse_version("1.2.3-beta.1"), Some([1, 2, 3]));
        assert_eq!(parse_version("1.2.3+build.7"), Some([1, 2, 3]));
        assert!(is_newer("0.9.0-beta.1", "0.8.0"));
        assert!(!is_newer("0.8.0-beta.1", "0.8.0"));
    }

    #[test]
    fn reads_the_release_response() {
        let json = r#"{
            "tag_name": "v0.9.0",
            "html_url": "https://example.test/releases/tag/v0.9.0",
            "draft": false,
            "prerelease": false
        }"#;
        let release: Release = serde_json::from_str(json).expect("valid release JSON");
        assert_eq!(release.tag_name, "v0.9.0");
        assert_eq!(release.url(), "https://example.test/releases/tag/v0.9.0");
    }

    #[test]
    fn missing_release_page_falls_back() {
        let release: Release =
            serde_json::from_str(r#"{"tag_name":"v0.9.0"}"#).expect("valid release JSON");
        assert_eq!(release.url(), RELEASES_URL);
    }

    #[test]
    fn finds_the_installer_and_checksums() {
        let json = r#"{"tag_name":"v0.9.0","assets":[
            {"name":"plic-0.9.0.zip","browser_download_url":"https://x/zip"},
            {"name":"plic-0.9.0-setup.exe","browser_download_url":"https://x/setup"},
            {"name":"SHA256SUMS.txt","browser_download_url":"https://x/sums"}]}"#;
        let release: Release = serde_json::from_str(json).expect("valid release JSON");
        assert_eq!(
            release.asset(|n| n.ends_with("-setup.exe")).map(|d| d.url),
            Some("https://x/setup".to_string())
        );
        assert!(release.asset(|n| n == "SHA256SUMS.txt").is_some());
    }

    #[test]
    fn reads_the_digest_github_reports() {
        let hex = "27b7f3cc31d72db7b41e46a7f09cc8dba606d52cffc727de5a8db722eb2a06b7";
        let json = format!(
            r#"{{"tag_name":"v0.9.0","assets":[
            {{"name":"plic-0.9.0-setup.exe","browser_download_url":"https://x/setup",
              "digest":"sha256:{}"}},
            {{"name":"plic-0.9.0.zip","browser_download_url":"https://x/zip","digest":null}},
            {{"name":"other","browser_download_url":"https://x/o","digest":"md5:abc"}}]}}"#,
            hex.to_ascii_uppercase()
        );
        let release: Release = serde_json::from_str(&json).expect("valid release JSON");
        let setup = release
            .asset(|n| n.ends_with("-setup.exe"))
            .expect("installer");
        assert_eq!(setup.sha256.as_deref(), Some(hex));
        assert_eq!(
            release
                .asset(|n| n.ends_with(".zip"))
                .and_then(|d| d.sha256),
            None
        );
        assert_eq!(release.asset(|n| n == "other").and_then(|d| d.sha256), None);
    }

    #[test]
    fn reads_checksums_from_a_sums_file() {
        let sums = "ABC123  plic-0.9.0-setup.exe\r\nff00 *other.zip\n";
        assert_eq!(
            expected_checksum(sums, "plic-0.9.0-setup.exe").as_deref(),
            Some("abc123")
        );
        assert_eq!(
            expected_checksum(sums, "other.zip").as_deref(),
            Some("ff00")
        );
        assert_eq!(expected_checksum(sums, "missing.exe"), None);
    }

    #[test]
    fn automatic_checks_wait_for_the_interval() {
        let now = unix_seconds();
        assert!(is_due_at(None, now));
        assert!(!is_due_at(Some(now), now));
        assert!(is_due_at(
            Some(now.saturating_sub(CHECK_INTERVAL.as_secs() + 1)),
            now
        ));
    }
}
