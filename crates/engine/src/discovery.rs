//! Finding GGUF files published after this version was built.
//!
//! PFN publishes one Hugging Face repository per model size, named
//! `pfnet/<family>-<size>-GGUF` with files `<family>-<size>-<quant>.gguf`. Discovery lists the
//! repositories of supported families, pins each to its current commit, and reads file sizes
//! and SHA-256 digests from the Git LFS metadata, so downloads stay verified.

use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::models::{CatalogEntry, SUPPORTED_FAMILIES};

const API: &str = "https://huggingface.co/api/models";
const AUTHOR: &str = "pfnet";

#[derive(Debug, Error)]
pub enum DiscoveryError {
    #[error("network error: {0}")]
    Http(String),
    #[error("unexpected response from Hugging Face: {0}")]
    Format(String),
}

/// A discovered file, with owned strings so it can be cached as JSON.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveredEntry {
    pub family: String,
    pub size: String,
    pub quant: String,
    pub repo: String,
    pub revision: String,
    pub file_name: String,
    pub bytes: u64,
    pub sha256: String,
    /// When the commit that last changed the file was made, and its title (see
    /// [`CatalogEntry::published`]). Caches written by versions before 0.5.0 have neither.
    #[serde(default)]
    pub published: Option<String>,
    #[serde(default)]
    pub change: Option<String>,
}

impl DiscoveredEntry {
    /// Whether the fields are what discovery derives from Hugging Face's metadata. The digest
    /// becomes part of a file name (the partial download), so it must be hex.
    fn is_valid(&self) -> bool {
        parse_repo(&self.repo)
            .is_some_and(|(family, size)| family == self.family && size == self.size)
            && parse_file(&self.file_name, &self.family, &self.size).as_deref()
                == Some(&*self.quant)
            && is_lower_hex(&self.sha256, 64)
            && is_lower_hex(&self.revision, 40)
            && self
                .published
                .as_deref()
                .is_none_or(|p| timestamp(p).as_deref() == Some(p))
    }

    /// Converts to a catalog entry. The strings are leaked, which is fine for the handful of
    /// entries an app run registers.
    pub fn into_entry(self) -> CatalogEntry {
        fn leak(s: String) -> &'static str {
            Box::leak(s.into_boxed_str())
        }
        CatalogEntry {
            family: leak(self.family),
            size: leak(self.size),
            quant: leak(self.quant),
            repo: leak(self.repo),
            revision: leak(self.revision),
            file_name: leak(self.file_name),
            bytes: self.bytes,
            sha256: leak(self.sha256),
            published: self.published.map(leak),
            change: self.change.map(leak),
        }
    }
}

#[derive(Deserialize)]
struct ModelSummary {
    id: String,
}

#[derive(Deserialize)]
struct ModelInfo {
    sha: String,
}

#[derive(Deserialize)]
struct TreeItem {
    #[serde(rename = "type")]
    kind: String,
    path: String,
    lfs: Option<LfsInfo>,
    #[serde(rename = "lastCommit")]
    last_commit: Option<LastCommit>,
}

/// The commit that last changed a file, listed with `expand=true`.
#[derive(Deserialize)]
struct LastCommit {
    title: String,
    date: String,
}

#[derive(Deserialize)]
struct LfsInfo {
    oid: String,
    size: u64,
}

/// Lists all GGUF files of supported families currently published by PFN.
pub fn discover() -> Result<Vec<DiscoveredEntry>, DiscoveryError> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(30)))
        .user_agent(crate::USER_AGENT)
        .build()
        .into();
    let models: Vec<ModelSummary> = get_json(
        &agent,
        &format!("{API}?author={AUTHOR}&search=translate&limit=500"),
    )?;
    let mut found = Vec::new();
    for model in models {
        let Some((family, size)) = parse_repo(&model.id) else {
            continue;
        };
        let info: ModelInfo = get_json(&agent, &format!("{API}/{}", model.id))?;
        let tree: Vec<TreeItem> = get_json(
            &agent,
            &format!("{API}/{}/tree/{}?expand=true", model.id, info.sha),
        )?;
        for item in tree {
            let (Some(lfs), true) = (item.lfs, item.kind == "file") else {
                continue;
            };
            let Some(quant) = parse_file(&item.path, &family, &size) else {
                continue;
            };
            let (published, change) = item
                .last_commit
                .and_then(|c| Some((timestamp(&c.date)?, c.title)))
                .unzip();
            found.push(DiscoveredEntry {
                family: family.clone(),
                size: size.clone(),
                quant,
                repo: model.id.clone(),
                revision: info.sha.clone(),
                file_name: item.path,
                bytes: lfs.size,
                sha256: lfs.oid,
                published,
                change,
            });
        }
    }
    // As for the cache: only what has the expected shape is registered.
    found.retain(DiscoveredEntry::is_valid);
    Ok(found)
}

fn get_json<T: serde::de::DeserializeOwned>(
    agent: &ureq::Agent,
    url: &str,
) -> Result<T, DiscoveryError> {
    let body = agent
        .get(url)
        .call()
        .map_err(|e| DiscoveryError::Http(e.to_string()))?
        .body_mut()
        .read_to_string()
        .map_err(|e| DiscoveryError::Http(e.to_string()))?;
    serde_json::from_str(&body).map_err(|e| DiscoveryError::Format(format!("{url}: {e}")))
}

/// `pfnet/plamo-3-translate-2b-GGUF` → (`plamo-3-translate`, `2b`) for supported families.
fn parse_repo(id: &str) -> Option<(String, String)> {
    let name = id
        .strip_prefix(&format!("{AUTHOR}/"))?
        .strip_suffix("-GGUF")?;
    let (family, size) = name.rsplit_once('-')?;
    let digits = size.strip_suffix('b')?;
    let is_size = !digits.is_empty()
        && digits.bytes().all(|b| b.is_ascii_digit() || b == b'.')
        && digits.parse::<f32>().is_ok();
    (is_size && SUPPORTED_FAMILIES.contains(&family)).then(|| (family.to_owned(), size.to_owned()))
}

/// `plamo-3-translate-2b-Q4_K_M.gguf` → `Q4_K_M`.
fn parse_file(path: &str, family: &str, size: &str) -> Option<String> {
    let quant = path
        .strip_prefix(&format!("{family}-{size}-"))?
        .strip_suffix(".gguf")?;
    let valid = !quant.is_empty()
        && quant
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_');
    valid.then(|| quant.to_owned())
}

/// Reads entries saved by [`save_cache`]. A missing or unreadable cache yields nothing, and
/// entries that a fresh discovery would not produce are dropped: the file name becomes a path
/// that is written and deleted.
pub fn load_cache(path: &Path) -> Vec<DiscoveredEntry> {
    let entries: Vec<DiscoveredEntry> = std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default();
    entries
        .into_iter()
        .filter(DiscoveredEntry::is_valid)
        .collect()
}

/// `2026-11-08T05:48:42.000Z` → `2026-11-08T05:48:42Z`, the form of the built-in catalog, so
/// dates compare as strings.
fn timestamp(date: &str) -> Option<String> {
    let (seconds, zone) = (date.get(..19)?, date.get(19..)?);
    let shape = seconds.bytes().enumerate().all(|(i, b)| match i {
        4 | 7 => b == b'-',
        10 => b == b'T',
        13 | 16 => b == b':',
        _ => b.is_ascii_digit(),
    });
    let utc = zone == "Z"
        || zone
            .strip_prefix('.')
            .and_then(|z| z.strip_suffix('Z'))
            .is_some_and(|f| !f.is_empty() && f.bytes().all(|b| b.is_ascii_digit()));
    (shape && utc).then(|| format!("{seconds}Z"))
}

fn is_lower_hex(s: &str, len: usize) -> bool {
    s.len() == len && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

pub fn save_cache(path: &Path, entries: &[DiscoveredEntry]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(entries)?)?;
    std::fs::rename(tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_repository_names() {
        assert_eq!(
            parse_repo("pfnet/plamo-3-translate-2b-GGUF"),
            Some(("plamo-3-translate".into(), "2b".into()))
        );
        assert_eq!(
            parse_repo("pfnet/plamo-3-translate-0.8b-GGUF"),
            Some(("plamo-3-translate".into(), "0.8b".into()))
        );
        for id in [
            "pfnet/plamo-3-translate-2b",
            "pfnet/plamo-3-translate-2b-FP8",
            "pfnet/plamo-2-translate-GGUF",
            "pfnet/plamo-9-chat-8b-GGUF",
            "someone/plamo-3-translate-2b-GGUF",
            "pfnet/plamo-3-translate-xb-GGUF",
        ] {
            assert_eq!(parse_repo(id), None, "{id}");
        }
    }

    #[test]
    fn parses_file_names() {
        let f = "plamo-3-translate";
        assert_eq!(
            parse_file("plamo-3-translate-2b-Q4_K_M.gguf", f, "2b").as_deref(),
            Some("Q4_K_M")
        );
        assert_eq!(
            parse_file("plamo-3-translate-2b-Q8_0.gguf", f, "2b").as_deref(),
            Some("Q8_0")
        );
        assert_eq!(parse_file("README.md", f, "2b"), None);
        assert_eq!(
            parse_file("plamo-3-translate-8b-Q4_K_M.gguf", f, "2b"),
            None
        );
        assert_eq!(
            parse_file("sub/plamo-3-translate-2b-Q4_K_M.gguf", f, "2b"),
            None
        );
    }

    #[test]
    fn cache_round_trips() {
        let path = std::env::temp_dir().join(format!("plamo-catalog-{}.json", std::process::id()));
        let entry = DiscoveredEntry {
            family: "plamo-3-translate".into(),
            size: "2b".into(),
            quant: "Q8_0".into(),
            repo: "pfnet/plamo-3-translate-2b-GGUF".into(),
            revision: "0".repeat(40),
            file_name: "plamo-3-translate-2b-Q8_0.gguf".into(),
            bytes: 42,
            sha256: "a".repeat(64),
            published: Some("2026-11-08T05:48:42Z".into()),
            change: Some("Update the chat template".into()),
        };
        save_cache(&path, std::slice::from_ref(&entry)).unwrap();
        assert_eq!(load_cache(&path), vec![entry.clone()]);
        // A cache of an earlier version, without dates, still loads.
        let mut old: serde_json::Value = serde_json::to_value([&entry]).unwrap();
        for e in old.as_array_mut().unwrap() {
            let e = e.as_object_mut().unwrap();
            e.remove("published");
            e.remove("change");
        }
        std::fs::write(&path, old.to_string()).unwrap();
        let loaded = load_cache(&path);
        assert_eq!(loaded.len(), 1);
        assert_eq!(
            (loaded[0].published.clone(), loaded[0].change.clone()),
            (None, None)
        );
        std::fs::remove_file(&path).unwrap();
        assert!(load_cache(&path).is_empty());
    }

    #[test]
    fn normalizes_timestamps() {
        assert_eq!(
            timestamp("2026-11-08T05:48:42.000Z").as_deref(),
            Some("2026-11-08T05:48:42Z")
        );
        assert_eq!(
            timestamp("2026-11-08T05:48:42Z").as_deref(),
            Some("2026-11-08T05:48:42Z")
        );
        for date in [
            "2026-11-08",
            "2026-11-08T05:48:42+09:00",
            "2026-11-08T05:48:42.Z",
            "2026/11/08T05:48:42Z",
            "",
        ] {
            assert_eq!(timestamp(date), None, "{date}");
        }
    }
}
