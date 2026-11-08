//! Newer releases of the app, from its GitHub releases.
//!
//! [`latest`] asks GitHub for the newest release. [`CheckCache`] keeps what the last check found,
//! so that the app asks at most once a day and still shows a newer release at every start.

use std::fmt;
use std::io;
use std::path::Path;
use std::str::FromStr;
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

/// The repository whose releases are the app's.
pub const REPOSITORY: &str = "tanakh/plamo-translate-desktop";

/// Replaces the address of the newest release in the GitHub API, for tests against a local
/// server.
pub const API_URL_VAR: &str = "PLAMO_TRANSLATE_UPDATE_URL";

/// How often the app asks GitHub at most.
pub const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

const USER_AGENT: &str = concat!("plamo-translate-desktop/", env!("CARGO_PKG_VERSION"));

/// A release's version, `major.minor.patch` (the tags are `v` and the same).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl Version {
    /// This build's version.
    pub fn current() -> Self {
        env!("CARGO_PKG_VERSION")
            .parse()
            .expect("the crate version is major.minor.patch")
    }
}

impl FromStr for Version {
    type Err = String;

    /// Takes `0.5.1` or `v0.5.1`. The releases have no pre-release or build suffixes, so a
    /// version with one is not taken.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let invalid = || format!("not a version: {s:?}");
        let mut parts = s.strip_prefix('v').unwrap_or(s).split('.');
        let mut next = || -> Result<u32, String> {
            let part = parts.next().ok_or_else(invalid)?;
            // No signs or leading zeros, which `parse` would take.
            if part.is_empty()
                || !part.bytes().all(|b| b.is_ascii_digit())
                || (part.len() > 1 && part.starts_with('0'))
            {
                return Err(invalid());
            }
            part.parse().map_err(|_| invalid())
        };
        let version = Self {
            major: next()?,
            minor: next()?,
            patch: next()?,
        };
        if parts.next().is_some() {
            return Err(invalid());
        }
        Ok(version)
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

impl TryFrom<String> for Version {
    type Error = String;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

impl From<Version> for String {
    fn from(version: Version) -> Self {
        version.to_string()
    }
}

/// A published release.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Release {
    pub version: Version,
    /// The release's page on GitHub, with its notes and files.
    pub page: String,
    pub assets: Vec<Asset>,
}

/// A file of a release.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub size: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    #[error("cannot reach GitHub: {0}")]
    Http(String),
    #[error("unexpected answer from GitHub: {0}")]
    Format(String),
}

/// Asks GitHub for the newest release (drafts and pre-releases are not given).
pub fn latest() -> Result<Release, UpdateError> {
    let url = std::env::var(API_URL_VAR)
        .unwrap_or_else(|_| format!("https://api.github.com/repos/{REPOSITORY}/releases/latest"));
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(30)))
        .user_agent(USER_AGENT)
        .build()
        .into();
    let body = agent
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .call()
        .map_err(|e| UpdateError::Http(e.to_string()))?
        .body_mut()
        .read_to_string()
        .map_err(|e| UpdateError::Http(e.to_string()))?;
    parse_release(&body)
}

/// Reads a release as the GitHub API gives it.
fn parse_release(json: &str) -> Result<Release, UpdateError> {
    #[derive(Deserialize)]
    struct ApiRelease {
        tag_name: String,
        html_url: String,
        #[serde(default)]
        assets: Vec<ApiAsset>,
    }
    #[derive(Deserialize)]
    struct ApiAsset {
        name: String,
        browser_download_url: String,
        size: u64,
    }
    let release: ApiRelease =
        serde_json::from_str(json).map_err(|e| UpdateError::Format(e.to_string()))?;
    Ok(Release {
        version: release.tag_name.parse().map_err(UpdateError::Format)?,
        page: release.html_url,
        assets: release
            .assets
            .into_iter()
            .map(|asset| Asset {
                name: asset.name,
                url: asset.browser_download_url,
                size: asset.size,
            })
            .collect(),
    })
}

/// What the last check found, kept between runs.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckCache {
    /// When GitHub was last asked, in seconds since the Unix epoch.
    pub checked_at: u64,
    /// The newest release then.
    pub release: Option<Release>,
}

impl CheckCache {
    /// Reads the cache at `path`; a missing or unreadable one is empty, so a check is due.
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self)?)?;
        std::fs::rename(tmp, path)
    }

    /// A cache of a check made `now`.
    pub fn checked(now: SystemTime, release: Release) -> Self {
        Self {
            checked_at: unix_seconds(now),
            release: Some(release),
        }
    }

    /// Whether GitHub should be asked again: a day has passed, or the clock was set back.
    pub fn is_due(&self, now: SystemTime) -> bool {
        let now = unix_seconds(now);
        now < self.checked_at || now - self.checked_at >= CHECK_INTERVAL.as_secs()
    }

    /// The release found, if it is newer than `current`.
    pub fn newer_than(&self, current: Version) -> Option<&Release> {
        self.release
            .as_ref()
            .filter(|release| release.version > current)
    }
}

fn unix_seconds(time: SystemTime) -> u64 {
    time.duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(major: u32, minor: u32, patch: u32) -> Version {
        Version {
            major,
            minor,
            patch,
        }
    }

    #[test]
    fn parses_versions() {
        assert_eq!("0.5.1".parse(), Ok(v(0, 5, 1)));
        assert_eq!("v10.0.12".parse(), Ok(v(10, 0, 12)));
        for bad in [
            "",
            "v",
            "0.5",
            "0.5.1.2",
            "0.5.x",
            "0.5.1-dev",
            "+1.0.0",
            "01.0.0",
            "0..1",
        ] {
            assert!(bad.parse::<Version>().is_err(), "{bad:?}");
        }
        assert_eq!(v(0, 5, 1).to_string(), "0.5.1");
        assert!(Version::current() >= v(0, 5, 0));
    }

    #[test]
    fn orders_versions_by_number() {
        assert!(v(0, 10, 0) > v(0, 9, 9));
        assert!(v(1, 0, 0) > v(0, 99, 99));
        assert!(v(0, 5, 1) > v(0, 5, 0));
    }

    #[test]
    fn reads_the_api_answer() {
        let json = r#"{
            "tag_name": "v0.5.1",
            "html_url": "https://github.com/tanakh/plamo-translate-desktop/releases/tag/v0.5.1",
            "draft": false,
            "assets": [
                {"name": "SHA256SUMS.txt", "size": 586, "browser_download_url": "https://example.com/SHA256SUMS.txt", "id": 1}
            ],
            "body": "notes"
        }"#;
        let release = parse_release(json).unwrap();
        assert_eq!(release.version, v(0, 5, 1));
        assert!(release.page.ends_with("/v0.5.1"));
        assert_eq!(
            release.assets,
            [Asset {
                name: "SHA256SUMS.txt".into(),
                url: "https://example.com/SHA256SUMS.txt".into(),
                size: 586,
            }]
        );
        assert!(matches!(
            parse_release(r#"{"tag_name": "nightly", "html_url": ""}"#),
            Err(UpdateError::Format(_))
        ));
        assert!(matches!(
            parse_release("rate limited"),
            Err(UpdateError::Format(_))
        ));
    }

    #[test]
    fn checks_once_a_day() {
        let start = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        assert!(CheckCache::default().is_due(start));
        let release = Release {
            version: v(0, 5, 1),
            page: String::new(),
            assets: Vec::new(),
        };
        let cache = CheckCache::checked(start, release);
        assert!(!cache.is_due(start + Duration::from_secs(60)));
        assert!(cache.is_due(start + CHECK_INTERVAL));
        // A clock set back does not hold the next check off.
        assert!(cache.is_due(start - Duration::from_secs(60)));
        assert!(cache.newer_than(v(0, 5, 0)).is_some());
        assert!(cache.newer_than(v(0, 5, 1)).is_none());
        assert!(cache.newer_than(v(0, 6, 0)).is_none());
    }

    #[test]
    fn keeps_the_cache_in_a_file() {
        let dir = std::env::temp_dir().join(format!("update-cache-test-{}", std::process::id()));
        let path = dir.join("update.json");
        assert_eq!(CheckCache::load(&path), CheckCache::default());
        let cache = CheckCache::checked(
            SystemTime::UNIX_EPOCH + Duration::from_secs(5),
            Release {
                version: v(0, 5, 1),
                page: "https://example.com".into(),
                assets: Vec::new(),
            },
        );
        cache.save(&path).unwrap();
        assert_eq!(CheckCache::load(&path), cache);
        std::fs::write(&path, "{not json").unwrap();
        assert_eq!(CheckCache::load(&path), CheckCache::default());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
