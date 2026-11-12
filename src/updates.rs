//! Checking GitHub for a newer release of plic.
//!
//! The check is one unauthenticated request to the releases API. The
//! version the user dismissed, and when the last check ran, live in the
//! state plic already keeps between runs.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::Deserialize;

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
}

impl Release {
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
