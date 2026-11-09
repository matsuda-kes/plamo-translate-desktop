//! Catalog of PLaMo Translate GGUF files, their published quality, and the local model store.
//!
//! The catalog starts with the files known when this version was built ([`BUILTIN`]). Entries
//! found on Hugging Face at run time (see [`crate::discovery`]) are added with [`register`]; they
//! may add new sizes or quantizations, or supersede a built-in file with a newer revision. A
//! downloaded file of an older revision stays usable (see [`ModelStore::state`]).

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

/// Model families whose prompt format (PFML3) this app speaks.
pub const SUPPORTED_FAMILIES: &[&str] = &["plamo-3-translate"];

/// One downloadable GGUF file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CatalogEntry {
    /// Model family, e.g. `plamo-3-translate`.
    pub family: &'static str,
    /// Size as written in file names, e.g. `2b` or `0.8b`.
    pub size: &'static str,
    /// GGUF quantization type, e.g. `Q4_K_M`.
    pub quant: &'static str,
    /// Hugging Face repository, e.g. `pfnet/plamo-3-translate-2b-GGUF`.
    pub repo: &'static str,
    /// Pinned repository commit, so the URL always serves the file `sha256` describes.
    pub revision: &'static str,
    pub file_name: &'static str,
    pub bytes: u64,
    /// Lowercase hex SHA-256 of the file.
    pub sha256: &'static str,
    /// When the commit that last changed the file was made (ISO 8601, UTC), if known. Hugging
    /// Face has no version numbers: this is the file's version.
    pub published: Option<&'static str>,
    /// That commit's title, which says what changed.
    pub change: Option<&'static str>,
}

impl CatalogEntry {
    /// Stable identifier: the file name without the `.gguf` extension.
    pub fn id(&self) -> &'static str {
        self.file_name
            .strip_suffix(".gguf")
            .unwrap_or(self.file_name)
    }

    /// `family-size`, e.g. `plamo-3-translate-2b`: the unquantized model this file comes from.
    pub fn base_model(&self) -> String {
        format!("{}-{}", self.family, self.size)
    }

    pub fn url(&self) -> String {
        format!(
            "https://huggingface.co/{}/resolve/{}/{}",
            self.repo, self.revision, self.file_name
        )
    }

    /// Parameter count in billions, parsed from the size (`0.8b` → 0.8).
    pub fn params_billion(&self) -> f32 {
        self.size
            .trim_end_matches(['b', 'B'])
            .parse()
            .unwrap_or(0.0)
    }

    /// The day the file was published (`2026-11-08`), if known.
    pub fn published_date(&self) -> Option<&'static str> {
        self.published.and_then(|p| p.get(..10))
    }

    /// Size for display, e.g. `2B`.
    pub fn size_label(&self) -> String {
        self.size.to_uppercase()
    }

    /// Whether this is the recommended quantization for its size.
    pub fn is_recommended_quant(&self) -> bool {
        self.quant == "Q4_K_M"
    }

    /// Published translation quality of this file, if known.
    pub fn quality(&self) -> Option<&'static Quality> {
        QUALITY
            .iter()
            .find(|(id, _)| *id == self.id())
            .map(|(_, q)| q)
    }

    /// Published quality of the unquantized (BF16) model of the same size, if known.
    pub fn reference_quality(&self) -> Option<&'static Quality> {
        let base = self.base_model();
        QUALITY.iter().find(|(id, _)| *id == base).map(|(_, q)| q)
    }
}

impl fmt::Display for CatalogEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} · {}", self.size_label(), self.quant)
    }
}

/// Translation quality published on the model card (higher is better).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Quality {
    /// PFN Translation Benchmark, English ↔ Japanese.
    pub pftb: f32,
    /// SaaS benchmark, English → Japanese.
    pub saas: f32,
    /// WMT24++ COMET-22, English → Japanese.
    pub wmt24pp: f32,
    /// FLORES+ chrF++, Japanese ↔ 14 languages.
    pub flores_ja: f32,
    /// FLORES+ chrF++, English ↔ 14 languages.
    pub flores_en: f32,
}

const fn q(pftb: f32, saas: f32, wmt24pp: f32, flores_ja: f32, flores_en: f32) -> Quality {
    Quality {
        pftb,
        saas,
        wmt24pp,
        flores_ja,
        flores_en,
    }
}

/// Scores from the "Accuracy changes from quantization" tables of the GGUF model cards.
/// Keys are file ids, plus `family-size` for the BF16 reference.
#[rustfmt::skip]
pub const QUALITY: &[(&str, Quality)] = &[
    ("plamo-3-translate-0.8b",        q(0.5862, 0.5530, 0.8724, 36.08, 54.44)),
    ("plamo-3-translate-0.8b-Q4_K_M", q(0.5832, 0.5451, 0.8716, 35.90, 54.47)),
    ("plamo-3-translate-0.8b-Q4_0",   q(0.5853, 0.5500, 0.8723, 34.97, 54.11)),
    ("plamo-3-translate-0.8b-Q3_K_M", q(0.5798, 0.5520, 0.8710, 35.32, 54.15)),
    ("plamo-3-translate-0.8b-Q2_K",   q(0.5620, 0.5397, 0.8659, 33.56, 51.15)),
    ("plamo-3-translate-2b",          q(0.6061, 0.5612, 0.8780, 37.02, 55.74)),
    ("plamo-3-translate-2b-Q4_K_M",   q(0.6062, 0.5602, 0.8774, 37.00, 55.76)),
    ("plamo-3-translate-2b-Q4_0",     q(0.6069, 0.5604, 0.8791, 36.12, 55.62)),
    ("plamo-3-translate-2b-Q3_K_M",   q(0.6053, 0.5644, 0.8789, 36.81, 55.59)),
    ("plamo-3-translate-2b-Q2_K",     q(0.5977, 0.5610, 0.8777, 36.45, 55.02)),
    ("plamo-3-translate-8b",          q(0.6119, 0.5750, 0.8805, 37.45, 56.22)),
    ("plamo-3-translate-8b-Q4_K_M",   q(0.6100, 0.5765, 0.8802, 37.39, 56.18)),
    ("plamo-3-translate-8b-Q3_K_M",   q(0.6019, 0.5707, 0.8826, 37.20, 55.93)),
    ("plamo-3-translate-8b-Q2_K",     q(0.5989, 0.5676, 0.8791, 36.54, 55.31)),
    ("plamo-3-translate-31b",         q(0.6244, 0.5786, 0.8799, 37.81, 56.61)),
    ("plamo-3-translate-31b-Q4_K_M",  q(0.6237, 0.5770, 0.8810, 37.84, 56.55)),
    ("plamo-3-translate-31b-Q3_K_M",  q(0.6223, 0.5780, 0.8813, 37.78, 56.54)),
    ("plamo-3-translate-31b-Q2_K",    q(0.6126, 0.5745, 0.8801, 37.36, 56.16)),
];

macro_rules! entry {
    ($slug:literal, $q:literal, $rev:literal, $bytes:literal, $sha:literal, $date:literal) => {
        CatalogEntry {
            family: "plamo-3-translate",
            size: $slug,
            quant: $q,
            repo: concat!("pfnet/plamo-3-translate-", $slug, "-GGUF"),
            revision: $rev,
            file_name: concat!("plamo-3-translate-", $slug, "-", $q, ".gguf"),
            bytes: $bytes,
            sha256: $sha,
            published: Some($date),
            change: Some(TARGET_LANG_AUTO),
        }
    };
}

/// What the commit of the files of [`BUILTIN`] changed. PFN has replaced every file four times
/// since the first release, each time changing only the chat template, which this app does not
/// use (it writes its own prompts).
const TARGET_LANG_AUTO: &str =
    "Update the chat template: switch between Japanese and English by default (target_lang=auto)";

/// The GGUF files published when this version was built (the revisions of 2026-11-08).
#[rustfmt::skip]
pub const BUILTIN: &[CatalogEntry] = &[
    entry!("0.8b", "Q4_K_M", "e10c327b7a7ce84ffc427cb4b09360b781429fda",   573126848, "70891ec70dc7ab4205745a38307867c2881b99239f20327927633abfe4bc9ec6", "2026-11-08T05:48:24Z"),
    entry!("0.8b", "Q4_0",   "e10c327b7a7ce84ffc427cb4b09360b781429fda",   547543232, "33deed2822f799de4d15bb03bea1ac40efa7d663d9239ee54cc7f65f5ab92917", "2026-11-08T05:48:24Z"),
    entry!("0.8b", "Q3_K_M", "e10c327b7a7ce84ffc427cb4b09360b781429fda",   486385856, "6c64dd53a367966ec00a7335ff34bb77ad5a893b6fbc7bf9ae51f7241e0dbfbe", "2026-11-08T05:48:24Z"),
    entry!("0.8b", "Q2_K",   "e10c327b7a7ce84ffc427cb4b09360b781429fda",   410138816, "97f0fd4a68d3d8eda39b018fd2f1ce2c58a6f86fad7dafd6d8b17cbdfab60709", "2026-11-08T05:48:24Z"),
    entry!("2b",   "Q4_K_M", "228a27cc0bb4f111219963848d50a3127c16ca73",  1807203040, "1ffd9073afcc70e35ccc942428aa2b73d7aa2b9ea779bf504b775369e0276c13", "2026-11-08T05:48:42Z"),
    entry!("2b",   "Q4_0",   "228a27cc0bb4f111219963848d50a3127c16ca73",  1698577120, "429b7934c88332262b4cca9fd5de36eb02e00f42dcd08ceb511a23e7ee4abafb", "2026-11-08T05:48:42Z"),
    entry!("2b",   "Q3_K_M", "228a27cc0bb4f111219963848d50a3127c16ca73",  1495886560, "fd79d48441cbb6eed8377084b43077ed006c9575ca55e47c002a8ee2f7e9faff", "2026-11-08T05:48:42Z"),
    entry!("2b",   "Q2_K",   "228a27cc0bb4f111219963848d50a3127c16ca73",  1225612000, "238232fe01810b5a0b42e93a4788bedb6b8831cc0baa4e1213ffb8394053b20d", "2026-11-08T05:48:42Z"),
    entry!("8b",   "Q4_K_M", "fccc6b15efc7d615189fa5df6a1a02ecd2dbc18f",  5280445728, "94e5d3f91b421f7c8900ddaac27b11a19f7441426c954bbb25a2fd60a1640187", "2026-11-08T05:49:07Z"),
    entry!("8b",   "Q3_K_M", "fccc6b15efc7d615189fa5df6a1a02ecd2dbc18f",  4310218016, "ddd9697162fe3c8e6ece064863ae694fb28430ee98c0addfbd241f56a7f45578", "2026-11-08T05:49:07Z"),
    entry!("8b",   "Q2_K",   "fccc6b15efc7d615189fa5df6a1a02ecd2dbc18f",  3450746144, "3322a15c142d2a30e552fe1ba054fa84174996c4537ee9161c1a1c96657dc127", "2026-11-08T05:49:07Z"),
    entry!("31b",  "Q4_K_M", "8aa0d99cd886de5c7efb1d9418ce56562f54946a", 19526593952, "47788c3eb71b830919a8454022381de943a43edfab65dad9e742997028d1f9df", "2026-11-08T05:50:19Z"),
    entry!("31b",  "Q3_K_M", "8aa0d99cd886de5c7efb1d9418ce56562f54946a", 15863131552, "2b91a6dc48a3544f08150e4e979a67af6ea489ac06ead39d0a3343bad2a2e330", "2026-11-08T05:50:19Z"),
    entry!("31b",  "Q2_K",   "8aa0d99cd886de5c7efb1d9418ce56562f54946a", 12609924512, "27b50ef2359e908aec72e0fc589535442eca8b17406c585d7f73db1d3e88cd65", "2026-11-08T05:50:19Z"),
];

/// Entries registered at run time. Each is leaked once, so `&'static` references stay valid.
static REGISTERED: RwLock<Vec<&'static CatalogEntry>> = RwLock::new(Vec::new());

/// Adds entries found at run time. An entry with the id of an existing one replaces it if it is
/// a newer revision; identical and older entries are ignored. Returns how many entries were
/// added or replaced.
pub fn register(entries: impl IntoIterator<Item = CatalogEntry>) -> usize {
    let mut registered = REGISTERED.write().unwrap_or_else(|e| e.into_inner());
    let mut changed = 0;
    for entry in entries {
        if !SUPPORTED_FAMILIES.contains(&entry.family) {
            continue;
        }
        let current = registered
            .iter()
            .rev()
            .find(|e| e.id() == entry.id())
            .copied()
            .or_else(|| BUILTIN.iter().find(|e| e.id() == entry.id()));
        if current.is_some_and(|c| c.sha256 == entry.sha256 || !is_newer(&entry, c)) {
            continue;
        }
        registered.retain(|e| e.id() != entry.id());
        registered.push(Box::leak(Box::new(entry)));
        changed += 1;
    }
    changed
}

/// Whether `entry` is a newer revision than `current`. An entry without a date (from a catalog
/// cached by a version before dates were kept) may be older than the built-in one, so it never
/// replaces a dated one.
fn is_newer(entry: &CatalogEntry, current: &CatalogEntry) -> bool {
    match (entry.published, current.published) {
        (Some(new), Some(old)) => new > old,
        (None, Some(_)) => false,
        (_, None) => true,
    }
}

/// All known entries, newest revision of each file, sorted by family, size and quantization.
pub fn entries() -> Vec<&'static CatalogEntry> {
    let registered = REGISTERED.read().unwrap_or_else(|e| e.into_inner());
    let mut all: Vec<&'static CatalogEntry> = BUILTIN
        .iter()
        .filter(|b| !registered.iter().any(|r| r.id() == b.id()))
        .chain(registered.iter().copied())
        .collect();
    all.sort_by(|a, b| {
        a.family
            .cmp(b.family)
            .then(a.params_billion().total_cmp(&b.params_billion()))
            .then(quant_rank(a.quant).cmp(&quant_rank(b.quant)))
            .then(a.quant.cmp(b.quant))
    });
    all
}

/// Display order of quantizations: higher precision first, then the published 4-bit default.
fn quant_rank(quant: &str) -> usize {
    const ORDER: &[&str] = &[
        "BF16", "F16", "Q8_0", "Q6_K", "Q5_K_M", "Q5_K_S", "Q5_0", "Q4_K_M", "Q4_K_S", "Q4_0",
        "Q3_K_M", "Q3_K_S", "Q2_K",
    ];
    ORDER
        .iter()
        .position(|q| *q == quant)
        .unwrap_or(ORDER.len())
}

/// The entry used when nothing else is known about the machine.
pub fn default_entry() -> &'static CatalogEntry {
    find_entry("plamo-3-translate-2b-Q4_K_M").expect("default model is in the catalog")
}

pub fn find_entry(id: &str) -> Option<&'static CatalogEntry> {
    entries().into_iter().find(|e| e.id() == id)
}

/// Picks the recommended model for the given amount of memory available to the model.
///
/// The model file plus working memory should fit with some headroom, so this asks for
/// roughly 1.5x the file size. Models above 8B are never recommended automatically.
pub fn recommend(available_bytes: u64) -> &'static CatalogEntry {
    let candidates: Vec<_> = entries()
        .into_iter()
        .filter(|e| e.family == SUPPORTED_FAMILIES[0] && e.is_recommended_quant())
        .filter(|e| e.params_billion() <= 8.0)
        .collect();
    candidates
        .iter()
        .filter(|e| e.bytes.saturating_mul(3) / 2 <= available_bytes)
        .max_by(|a, b| a.params_billion().total_cmp(&b.params_billion()))
        .or_else(|| {
            candidates
                .iter()
                .min_by(|a, b| a.params_billion().total_cmp(&b.params_billion()))
        })
        .copied()
        .unwrap_or_else(default_entry)
}

/// Local state of a catalog entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalState {
    /// The file of the newest known revision is present.
    Downloaded,
    /// A file of another revision is present, and is used as it is. The newest revision can be
    /// downloaded beside it, and `partial` bytes of it already are.
    Outdated {
        partial: Option<u64>,
    },
    /// A partial download of this many bytes can be resumed.
    Partial(u64),
    Missing,
}

impl LocalState {
    /// Whether there is a file to load.
    pub fn is_usable(self) -> bool {
        matches!(self, Self::Downloaded | Self::Outdated { .. })
    }

    /// How much of the newest revision is downloaded, if a download of it can be resumed.
    pub fn partial(self) -> Option<u64> {
        match self {
            Self::Partial(n) => Some(n),
            Self::Outdated { partial } => partial,
            Self::Downloaded | Self::Missing => None,
        }
    }
}

/// Directory holding downloaded models.
#[derive(Debug, Clone)]
pub struct ModelStore {
    dir: PathBuf,
}

impl ModelStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Final location of a downloaded model.
    pub fn path(&self, entry: &CatalogEntry) -> PathBuf {
        self.dir.join(entry.file_name)
    }

    /// Location of an in-progress download.
    pub fn partial_path(&self, entry: &CatalogEntry) -> PathBuf {
        // Named after the expected digest too, so a partial download of an older revision is
        // never resumed against a newer one.
        let digest = entry.sha256.get(..12).unwrap_or(entry.sha256);
        self.dir.join(format!("{}.{digest}.part", entry.file_name))
    }

    /// Downloads only reach their final name after verification, so a final file is taken as
    /// complete; one of the newest revision's size counts as that revision. A file of another
    /// size is another revision (downloaded earlier, or copied in by hand), used as it is: the
    /// revisions so far differ only in their chat template, which this app does not use.
    pub fn state(&self, entry: &CatalogEntry) -> LocalState {
        let partial = std::fs::metadata(self.partial_path(entry))
            .ok()
            .map(|m| m.len())
            .filter(|&n| n > 0);
        match std::fs::metadata(self.path(entry)) {
            Ok(m) if m.is_file() && m.len() == entry.bytes => LocalState::Downloaded,
            Ok(m) if m.is_file() && m.len() > 0 => LocalState::Outdated { partial },
            _ => partial.map_or(LocalState::Missing, LocalState::Partial),
        }
    }

    /// Partial downloads of `entry`'s file for other revisions (another digest in the name):
    /// they are never resumed, since only the newest revision is downloaded.
    pub fn stale_partials(&self, entry: &CatalogEntry) -> Vec<PathBuf> {
        let current = self.partial_path(entry);
        let prefix = format!("{}.", entry.file_name);
        let Ok(dir) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        dir.flatten()
            .map(|e| e.path())
            .filter(|path| *path != current)
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .and_then(|name| name.strip_prefix(&prefix)?.strip_suffix(".part"))
                    .is_some_and(|digest| {
                        digest.len() == 12 && digest.bytes().all(|b| b.is_ascii_hexdigit())
                    })
            })
            .collect()
    }

    /// Removes the stale partial downloads of `entry` (see [`Self::stale_partials`]), returning
    /// the bytes freed.
    pub fn remove_stale_partials(&self, entry: &CatalogEntry) -> u64 {
        self.stale_partials(entry)
            .into_iter()
            .filter_map(|path| {
                let len = std::fs::metadata(&path).ok()?.len();
                std::fs::remove_file(&path).ok().map(|()| len)
            })
            .sum()
    }

    /// Puts the newest revision of `entry`, downloaded and verified beside an older one (see
    /// [`crate::download::Downloaded::BesideOlder`]), in place of the older one. Fails while the
    /// older file is in use on Windows.
    pub fn replace_with_download(&self, entry: &CatalogEntry) -> io::Result<PathBuf> {
        let path = self.path(entry);
        std::fs::rename(self.partial_path(entry), &path)?;
        Ok(path)
    }

    /// Deletes the model and any partial download, of this revision or an older one.
    pub fn delete(&self, entry: &CatalogEntry) -> io::Result<()> {
        for path in [self.path(entry), self.partial_path(entry)]
            .into_iter()
            .chain(self.stale_partials(entry))
        {
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_catalog_is_consistent() {
        assert_eq!(BUILTIN.len(), 14);
        for e in BUILTIN {
            assert_eq!(
                e.file_name,
                format!("{}-{}-{}.gguf", e.family, e.size, e.quant)
            );
            assert_eq!(e.sha256.len(), 64);
            assert!(e.sha256.bytes().all(|b| b.is_ascii_hexdigit()));
            assert_eq!(e.revision.len(), 40);
            assert!(e.published_date().is_some() && e.change.is_some());
            assert!(e.quality().is_some(), "{} has no quality data", e.id());
            assert!(e.reference_quality().is_some());
        }
        assert_eq!(default_entry().to_string(), "2B · Q4_K_M");
        assert_eq!(default_entry().params_billion(), 2.0);
    }

    #[test]
    fn url_points_to_pinned_revision() {
        let e = default_entry();
        assert_eq!(
            e.url(),
            "https://huggingface.co/pfnet/plamo-3-translate-2b-GGUF/resolve/\
             228a27cc0bb4f111219963848d50a3127c16ca73/plamo-3-translate-2b-Q4_K_M.gguf"
        );
    }

    #[test]
    fn entries_are_sorted_by_size_then_quantization() {
        let ids: Vec<_> = entries().iter().map(|e| e.id()).take(5).collect();
        assert_eq!(
            ids,
            [
                "plamo-3-translate-0.8b-Q4_K_M",
                "plamo-3-translate-0.8b-Q4_0",
                "plamo-3-translate-0.8b-Q3_K_M",
                "plamo-3-translate-0.8b-Q2_K",
                "plamo-3-translate-2b-Q4_K_M",
            ]
        );
    }

    #[test]
    fn registered_entries_add_and_supersede() {
        let base = *find_entry("plamo-3-translate-8b-Q4_K_M").unwrap();
        // A new quantization appears.
        let q8 = CatalogEntry {
            quant: "Q8_0",
            file_name: "plamo-3-translate-8b-Q8_0.gguf",
            sha256: "aa",
            ..base
        };
        // The same file at a newer revision.
        let newer = CatalogEntry {
            revision: "ffffffffffffffffffffffffffffffffffffffff",
            sha256: "bb",
            published: Some("2099-01-01T00:00:00Z"),
            ..base
        };
        // An unsupported family is ignored.
        let other = CatalogEntry {
            family: "plamo-9-chat",
            file_name: "plamo-9-chat-8b-Q4_K_M.gguf",
            ..base
        };
        // Registering a built-in entry unchanged is a no-op.
        assert_eq!(register([base]), 0);
        assert_eq!(register([q8, newer, other]), 2);
        assert_eq!(register([q8]), 0);
        let found = find_entry("plamo-3-translate-8b-Q4_K_M").unwrap();
        assert_eq!(found.sha256, "bb");
        let order: Vec<_> = entries()
            .iter()
            .filter(|e| e.size == "8b")
            .map(|e| e.quant)
            .collect();
        assert_eq!(order, ["Q8_0", "Q4_K_M", "Q3_K_M", "Q2_K"]);
        assert!(find_entry("plamo-9-chat-8b-Q4_K_M").is_none());
    }

    #[test]
    fn recommends_by_available_memory() {
        const GB: u64 = 1 << 30;
        assert_eq!(recommend(16 * GB).size, "8b");
        assert_eq!(recommend(4 * GB).size, "2b");
        assert_eq!(recommend(GB).size, "0.8b");
        assert_eq!(recommend(0).size, "0.8b");
        // 31B is never recommended automatically.
        assert_eq!(recommend(128 * GB).size, "8b");
    }

    #[test]
    fn store_reports_local_state() {
        let dir = std::env::temp_dir().join(format!("plamo-store-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = ModelStore::new(&dir);
        let e = &BUILTIN[0];
        assert_eq!(store.state(e), LocalState::Missing);
        std::fs::write(store.partial_path(e), b"abc").unwrap();
        assert_eq!(store.state(e), LocalState::Partial(3));
        assert!(!store.state(e).is_usable());
        // A final file of another size is another revision: usable, and the newest one's
        // partial download can be resumed beside it.
        std::fs::write(store.path(e), b"older").unwrap();
        assert_eq!(store.state(e), LocalState::Outdated { partial: Some(3) });
        assert!(store.state(e).is_usable());
        assert_eq!(store.state(e).partial(), Some(3));
        std::fs::remove_file(store.partial_path(e)).unwrap();
        assert_eq!(store.state(e), LocalState::Outdated { partial: None });
        // The newest revision's size (a sparse file).
        std::fs::File::create(store.path(e))
            .unwrap()
            .set_len(e.bytes)
            .unwrap();
        assert_eq!(store.state(e), LocalState::Downloaded);
        // An empty file is nothing.
        std::fs::write(store.path(e), b"").unwrap();
        assert_eq!(store.state(e), LocalState::Missing);
        store.delete(e).unwrap();
        assert_eq!(store.state(e), LocalState::Missing);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn stale_partials_are_removed() {
        let dir = std::env::temp_dir().join(format!("plamo-stale-parts-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = ModelStore::new(&dir);
        let e = &BUILTIN[0];
        // A partial download of an older revision, the current one, and other files.
        let old = dir.join(format!("{}.0123456789ab.part", e.file_name));
        std::fs::write(&old, b"old").unwrap();
        std::fs::write(store.partial_path(e), b"current").unwrap();
        std::fs::write(dir.join(format!("{}.notes.part", e.file_name)), b"x").unwrap();
        std::fs::write(
            dir.join(format!("{}.0123456789ab.part", BUILTIN[1].file_name)),
            b"y",
        )
        .unwrap();
        assert_eq!(store.stale_partials(e), vec![old.clone()]);
        assert_eq!(store.remove_stale_partials(e), 3);
        assert!(!old.exists());
        assert_eq!(store.state(e), LocalState::Partial(7));
        // Deleting removes older revisions' parts too.
        std::fs::write(&old, b"old").unwrap();
        store.delete(e).unwrap();
        assert!(!old.exists() && !store.partial_path(e).exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn older_revisions_are_ignored() {
        let current = *find_entry("plamo-3-translate-31b-Q2_K").unwrap();
        // The revision before the built-in one.
        let older = CatalogEntry {
            revision: "7970aee3005211df5d90740a3e009a9b26be4df9",
            bytes: 12609920864,
            sha256: "37d16404bc852dc8bf8ca95d6411e90b3bc0ff754b3c68474736ea59be3959ab",
            published: Some("2026-11-06T00:50:07Z"),
            ..current
        };
        // From a catalog cached before dates were kept: it may be older.
        let undated = CatalogEntry {
            published: None,
            change: None,
            ..older
        };
        assert_eq!(register([older, undated]), 0);
        assert_eq!(find_entry(current.id()).unwrap().sha256, current.sha256);
    }
}
