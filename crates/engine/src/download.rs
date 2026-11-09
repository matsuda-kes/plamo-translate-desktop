//! Resumable, verified model downloads.
//!
//! A download writes to `<file>.part`, resumes from its length with an HTTP `Range` request,
//! verifies the SHA-256 of the whole file, and only then renames it to its final name. A file of
//! an older revision with that name is left for the caller to replace, since it may be in use.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::models::{CatalogEntry, ModelStore};

const BUFFER_SIZE: usize = 1 << 20;
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Progress {
    pub downloaded: u64,
    pub total: u64,
    /// Recent transfer rate (exponentially smoothed), for display. Zero until measured.
    pub bytes_per_second: f64,
    /// Average rate since this transfer started. Steadier than the recent rate, so the
    /// ETA uses it once a few seconds have passed. Zero until then.
    pub average_bytes_per_second: f64,
}

impl Progress {
    pub fn fraction(&self) -> f32 {
        if self.total == 0 {
            0.0
        } else {
            (self.downloaded as f64 / self.total as f64) as f32
        }
    }

    /// Estimated time remaining, once a rate is known.
    pub fn eta(&self) -> Option<Duration> {
        let rate = if self.average_bytes_per_second > 0.0 {
            self.average_bytes_per_second
        } else {
            self.bytes_per_second
        };
        (rate > 0.0).then(|| {
            Duration::from_secs_f64(self.total.saturating_sub(self.downloaded) as f64 / rate)
        })
    }
}

#[derive(Debug, Error)]
pub enum DownloadError {
    #[error("cancelled")]
    Cancelled,
    #[error("network error: {0}")]
    Http(String),
    #[error("unexpected HTTP status {0}")]
    Status(u16),
    #[error("file error: {0}")]
    Io(#[from] io::Error),
    #[error("download ended early ({downloaded} of {total} bytes)")]
    Incomplete { downloaded: u64, total: u64 },
    #[error("checksum mismatch (expected {expected}, got {actual})")]
    ChecksumMismatch { expected: String, actual: String },
}

impl From<ureq::Error> for DownloadError {
    fn from(e: ureq::Error) -> Self {
        match e {
            ureq::Error::StatusCode(code) => Self::Status(code),
            ureq::Error::Io(e) => Self::Io(e),
            other => Self::Http(other.to_string()),
        }
    }
}

/// What a finished download left.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Downloaded {
    /// The file, at its final path.
    Placed(PathBuf),
    /// The verified file, still at its partial path: a file of an older revision has the final
    /// name, and may be in use (Windows cannot replace a loaded model).
    /// [`ModelStore::replace_with_download`] puts the new one in its place.
    BesideOlder,
}

/// Downloads `entry` into `store`, blocking the current thread.
///
/// `is_cancelled` is polled between reads. A cancelled or interrupted download keeps its
/// partial file so the next call resumes it.
pub fn download(
    entry: &CatalogEntry,
    store: &ModelStore,
    on_progress: &mut dyn FnMut(Progress),
    is_cancelled: &dyn Fn() -> bool,
) -> Result<Downloaded, DownloadError> {
    std::fs::create_dir_all(store.dir())?;
    store.remove_stale_partials(entry);
    let part_path = store.partial_path(entry);
    let total = entry.bytes;

    let mut part = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&part_path)?;
    let mut offset = part.metadata()?.len();
    if offset > total {
        part.set_len(0)?;
        offset = 0;
    }

    // Hash what is already on disk so the final digest covers the whole file.
    let mut hasher = Sha256::new();
    if offset > 0 {
        hash_prefix(&mut part, offset, &mut hasher, is_cancelled)?;
    }

    if offset < total {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_connect(Some(Duration::from_secs(20)))
            .timeout_recv_response(Some(Duration::from_secs(60)))
            // A stalled connection would otherwise block in `read` forever, past cancellation.
            .timeout_per_read(Some(Duration::from_secs(30)))
            .user_agent(crate::USER_AGENT)
            .build()
            .into();
        let mut request = agent.get(entry.url());
        if offset > 0 {
            request = request.header("Range", format!("bytes={offset}-"));
        }
        let mut response = request.call()?;
        match response.status().as_u16() {
            206 => {}
            200 => {
                // The server ignored the range: start over.
                part.set_len(0)?;
                offset = 0;
                hasher = Sha256::new();
            }
            code => return Err(DownloadError::Status(code)),
        }
        part.seek(SeekFrom::Start(offset))?;

        // Never write past the expected size, whatever the server sends.
        let mut reader = response.body_mut().as_reader().take(total - offset);
        let mut buf = vec![0; BUFFER_SIZE];
        let mut downloaded = offset;
        let mut meter = RateMeter::new(downloaded);
        on_progress(Progress {
            downloaded,
            total,
            bytes_per_second: 0.0,
            average_bytes_per_second: 0.0,
        });
        loop {
            if is_cancelled() {
                part.flush()?;
                return Err(DownloadError::Cancelled);
            }
            let n = match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e.into()),
            };
            part.write_all(&buf[..n])?;
            hasher.update(&buf[..n]);
            downloaded += n as u64;
            if let Some((rate, average)) = meter.sample(downloaded) {
                on_progress(Progress {
                    downloaded,
                    total,
                    bytes_per_second: rate,
                    average_bytes_per_second: average,
                });
            }
        }
        if downloaded != total {
            return Err(DownloadError::Incomplete { downloaded, total });
        }
        on_progress(Progress {
            downloaded,
            total,
            bytes_per_second: meter.rate,
            average_bytes_per_second: meter.average(downloaded),
        });
    }

    part.sync_all()?;
    drop(part);
    let actual = hex(&hasher.finalize());
    if actual != entry.sha256 {
        std::fs::remove_file(&part_path)?;
        return Err(DownloadError::ChecksumMismatch {
            expected: entry.sha256.to_owned(),
            actual,
        });
    }
    let path = store.path(entry);
    if path.exists() {
        return Ok(Downloaded::BesideOlder);
    }
    std::fs::rename(&part_path, &path)?;
    Ok(Downloaded::Placed(path))
}

fn hash_prefix(
    file: &mut File,
    len: u64,
    hasher: &mut Sha256,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<(), DownloadError> {
    file.seek(SeekFrom::Start(0))?;
    let mut reader = file.take(len);
    let mut buf = vec![0; BUFFER_SIZE];
    loop {
        if is_cancelled() {
            return Err(DownloadError::Cancelled);
        }
        let n = reader.read(&mut buf)?;
        if n == 0 {
            return Ok(());
        }
        hasher.update(&buf[..n]);
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Throttles progress reports and measures the transfer rate.
struct RateMeter {
    start_time: Instant,
    start_bytes: u64,
    last_time: Instant,
    last_bytes: u64,
    rate: f64,
}

/// The average rate is reported only after this long, when it has settled.
const AVERAGE_WARMUP: Duration = Duration::from_secs(3);

impl RateMeter {
    fn new(bytes: u64) -> Self {
        let now = Instant::now();
        Self {
            start_time: now,
            start_bytes: bytes,
            last_time: now,
            last_bytes: bytes,
            rate: 0.0,
        }
    }

    /// Average rate since the start, or zero during the warm-up.
    fn average(&self, bytes: u64) -> f64 {
        let elapsed = self.start_time.elapsed();
        if elapsed < AVERAGE_WARMUP {
            0.0
        } else {
            (bytes - self.start_bytes) as f64 / elapsed.as_secs_f64()
        }
    }

    /// Returns the recent and average rates when a report is due.
    fn sample(&mut self, bytes: u64) -> Option<(f64, f64)> {
        let elapsed = self.last_time.elapsed();
        if elapsed < PROGRESS_INTERVAL {
            return None;
        }
        let instant = (bytes - self.last_bytes) as f64 / elapsed.as_secs_f64();
        self.rate = if self.rate == 0.0 {
            instant
        } else {
            0.8 * self.rate + 0.2 * instant
        };
        self.last_time = Instant::now();
        self.last_bytes = bytes;
        Some((self.rate, self.average(bytes)))
    }
}

#[derive(Debug)]
pub enum DownloadEvent {
    Progress {
        entry: &'static CatalogEntry,
        progress: Progress,
    },
    Finished {
        entry: &'static CatalogEntry,
        path: PathBuf,
    },
    /// Downloaded and verified beside a file of an older revision, which may be in use:
    /// [`ModelStore::replace_with_download`] puts it in place.
    Verified {
        entry: &'static CatalogEntry,
    },
    Cancelled {
        entry: &'static CatalogEntry,
    },
    Failed {
        entry: &'static CatalogEntry,
        error: String,
    },
}

type EventSink = Arc<dyn Fn(DownloadEvent) + Send + Sync>;

/// Runs downloads on background threads, one per file.
pub struct Downloads {
    store: ModelStore,
    events: EventSink,
    active: Arc<Mutex<HashMap<&'static str, Arc<AtomicBool>>>>,
}

impl Downloads {
    pub fn new(store: ModelStore, events: impl Fn(DownloadEvent) + Send + Sync + 'static) -> Self {
        Self {
            store,
            events: Arc::new(events),
            active: Arc::default(),
        }
    }

    pub fn store(&self) -> &ModelStore {
        &self.store
    }

    /// Downloads from now on go to `store`; those running finish where they started.
    pub fn set_store(&mut self, store: ModelStore) {
        self.store = store;
    }

    pub fn is_active(&self, entry: &CatalogEntry) -> bool {
        self.active.lock().unwrap().contains_key(entry.id())
    }

    /// Starts downloading `entry`, unless it is already in progress.
    pub fn start(&self, entry: &'static CatalogEntry) {
        let cancel = Arc::new(AtomicBool::new(false));
        {
            let mut active = self.active.lock().unwrap();
            if active.contains_key(entry.id()) {
                return;
            }
            active.insert(entry.id(), cancel.clone());
        }
        let store = self.store.clone();
        let events = self.events.clone();
        let active = self.active.clone();
        thread::Builder::new()
            .name(format!("download {}", entry.id()))
            .spawn(move || {
                let result = download(
                    entry,
                    &store,
                    &mut |progress| events(DownloadEvent::Progress { entry, progress }),
                    &|| cancel.load(Ordering::Relaxed),
                );
                active.lock().unwrap().remove(entry.id());
                events(match result {
                    Ok(Downloaded::Placed(path)) => DownloadEvent::Finished { entry, path },
                    Ok(Downloaded::BesideOlder) => DownloadEvent::Verified { entry },
                    Err(DownloadError::Cancelled) => DownloadEvent::Cancelled { entry },
                    Err(e) => DownloadEvent::Failed {
                        entry,
                        error: e.to_string(),
                    },
                });
            })
            .expect("failed to spawn a download thread");
    }

    pub fn cancel(&self, entry: &CatalogEntry) {
        if let Some(flag) = self.active.lock().unwrap().get(entry.id()) {
            flag.store(true, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_math() {
        let p = Progress {
            downloaded: 25,
            total: 100,
            bytes_per_second: 5.0,
            average_bytes_per_second: 0.0,
        };
        assert!((p.fraction() - 0.25).abs() < 1e-6);
        // Before the average settles, the recent rate gives the ETA.
        assert_eq!(p.eta(), Some(Duration::from_secs(15)));
        // Afterwards the average wins.
        let p = Progress {
            average_bytes_per_second: 3.0,
            ..p
        };
        assert_eq!(p.eta(), Some(Duration::from_secs(25)));
        let p = Progress {
            bytes_per_second: 0.0,
            average_bytes_per_second: 0.0,
            ..p
        };
        assert_eq!(p.eta(), None);
    }

    #[test]
    fn hex_encoding() {
        assert_eq!(hex(&[0x00, 0xab, 0x10]), "00ab10");
        assert_eq!(
            hex(&Sha256::digest(b"")),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }
}
