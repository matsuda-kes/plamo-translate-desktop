//! Preparing an update: the installed package, the release's matching file, its download,
//! verification against `SHA256SUMS.txt`, and unpacking next to the app.
//!
//! Everything goes into a staging folder next to the installation ([`Installation::staging`]):
//! `new/` holds the new package's entries, `ready.json` says which version they are, and the
//! swap ([`crate::apply`]) moves the replaced entries into `old-*` folders, removed at a later
//! start by [`Installation::clean_up`].

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{Release, USER_AGENT, UpdateError, Version};

/// The file the release workflow puts in each package: next to the executables, or in
/// `Contents/Resources` of the macOS app.
pub const MARKER: &str = "package.json";

/// The checksums of a release's files.
pub const SUMS: &str = "SHA256SUMS.txt";

pub(crate) const READY: &str = "ready.json";
pub(crate) const FAILED: &str = "failed.txt";
const NEW: &str = "new";

/// What `package.json` says.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageInfo {
    /// The package's kind, as in its file name: `windows-x86_64`, `windows-x86_64-intel-npu`,
    /// `linux-x86_64`, or `macos-arm64`.
    pub kind: String,
    /// The package's top-level files and folders (Windows and Linux). An update replaces these
    /// only: anything else in the folder, such as a model kept there, stays.
    #[serde(default)]
    pub entries: Vec<String>,
}

impl PackageInfo {
    pub fn read(path: &Path) -> Result<Self, UpdateError> {
        let json = fs::read_to_string(path).map_err(|e| io_error(path, e))?;
        let info: Self = serde_json::from_str(&json)
            .map_err(|e| UpdateError::Format(format!("{}: {e}", path.display())))?;
        // The entries become paths: one that could leave the folder is not taken.
        if info.entries.iter().any(|entry| !is_plain_name(entry)) {
            return Err(UpdateError::Format(format!(
                "{}: an entry is not a plain name",
                path.display()
            )));
        }
        Ok(info)
    }
}

/// An installed copy of the app, from a package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installation {
    pub package: PackageInfo,
    /// The folder holding what an update replaces: the app's folder, or on macOS the folder
    /// holding the app.
    pub root: PathBuf,
    /// The names in `root` that an update replaces: the package's entries, or on macOS the app.
    pub entries: Vec<String>,
    /// The running executable.
    pub executable: PathBuf,
    /// The app bundle on macOS.
    pub bundle: Option<PathBuf>,
}

impl Installation {
    /// The installation of the running executable. A build from source has no `package.json`,
    /// and cannot be updated this way.
    pub fn current() -> Result<Self, UpdateError> {
        let exe = std::env::current_exe()
            .and_then(|exe| exe.canonicalize())
            .map_err(|e| UpdateError::Io(format!("cannot find the running executable: {e}")))?;
        Self::of_executable(&exe, cfg!(target_os = "macos"))
    }

    /// The installation of `exe`, laid out as on macOS (inside an app bundle) or not.
    pub fn of_executable(exe: &Path, macos: bool) -> Result<Self, UpdateError> {
        let not_packaged = || UpdateError::NotPackaged(exe.display().to_string());
        let dir = exe.parent().ok_or_else(not_packaged)?;
        if macos {
            // …/Name.app/Contents/MacOS/executable
            let contents = dir.parent().ok_or_else(not_packaged)?;
            let bundle = contents.parent().ok_or_else(not_packaged)?;
            if bundle.extension().is_none_or(|ext| ext != "app") {
                return Err(not_packaged());
            }
            let marker = contents.join("Resources").join(MARKER);
            if !marker.is_file() {
                return Err(not_packaged());
            }
            let name = bundle.file_name().ok_or_else(not_packaged)?;
            Ok(Self {
                package: PackageInfo::read(&marker)?,
                root: bundle.parent().ok_or_else(not_packaged)?.to_path_buf(),
                entries: vec![name.to_string_lossy().into_owned()],
                executable: exe.to_path_buf(),
                bundle: Some(bundle.to_path_buf()),
            })
        } else {
            let marker = dir.join(MARKER);
            if !marker.is_file() {
                return Err(not_packaged());
            }
            let package = PackageInfo::read(&marker)?;
            Ok(Self {
                entries: package.entries.clone(),
                package,
                root: dir.to_path_buf(),
                executable: exe.to_path_buf(),
                bundle: None,
            })
        }
    }

    fn is_macos(&self) -> bool {
        self.bundle.is_some()
    }

    /// The name of this installation's package in a release of `version`, such as
    /// `plamo-translate-desktop-0.5.1-windows-x86_64.zip`.
    pub fn asset_name(&self, version: Version) -> String {
        let kind = &self.package.kind;
        let extension = if kind.starts_with("windows") {
            "zip"
        } else if kind.starts_with("macos") {
            "dmg"
        } else {
            "tar.gz"
        };
        format!("plamo-translate-desktop-{version}-{kind}.{extension}")
    }

    /// Where an update is prepared: `update` in the app's folder, or a hidden folder next to the
    /// macOS app. On the same volume as the installation, so that the swap only renames.
    pub fn staging(&self) -> PathBuf {
        if self.is_macos() {
            self.root.join(".plamo-translate-update")
        } else {
            self.root.join("update")
        }
    }

    /// The prepared package's entries.
    pub(crate) fn new_dir(&self) -> PathBuf {
        self.staging().join(NEW)
    }

    /// Downloads `release`'s package for this installation, checks it against the release's
    /// `SHA256SUMS.txt`, and unpacks it into the staging folder. `progress(done, total)` follows
    /// the download.
    pub fn prepare(
        &self,
        release: &Release,
        progress: &mut dyn FnMut(u64, u64),
    ) -> Result<(), UpdateError> {
        let name = self.asset_name(release.version);
        let asset = release
            .assets
            .iter()
            .find(|asset| asset.name == name)
            .ok_or_else(|| UpdateError::NoPackage(name.clone()))?;
        let sums = release
            .assets
            .iter()
            .find(|asset| asset.name == SUMS)
            .ok_or_else(|| UpdateError::NoPackage(SUMS.into()))?;

        let staging = self.staging();
        // What an earlier attempt left, except the `old-*` folders of a finished update, which
        // may still be in use.
        self.clean_up(release.version);
        remove_if_exists(&self.new_dir())?;
        fs::create_dir_all(&staging).map_err(|e| UpdateError::NotWritable(e.to_string()))?;

        let agent = agent();
        let sums = agent
            .get(&sums.url)
            .call()
            .map_err(|e| UpdateError::Http(e.to_string()))?
            .body_mut()
            .read_to_string()
            .map_err(|e| UpdateError::Http(e.to_string()))?;
        let expected = checksum_of(&sums, &name)
            .ok_or_else(|| UpdateError::Format(format!("{SUMS} has no line for {name}")))?;

        let download = staging.join(&name);
        let actual = download_to(&agent, &asset.url, asset.size, &download, progress)?;
        if actual != expected {
            let _ = fs::remove_file(&download);
            return Err(UpdateError::Checksum(name));
        }

        let new = self.new_dir();
        let unpacked = if self.is_macos() {
            unpack_dmg(&download, &new)
        } else if name.ends_with(".zip") {
            unpack_zip(&download, &new)
        } else {
            unpack_tar_gz(&download, &new)
        };
        let _ = fs::remove_file(&download);
        unpacked?;

        // The new package must be the same kind as this one.
        let marker = match self.is_macos() {
            true => new_bundle(&new)?.join("Contents/Resources").join(MARKER),
            false => new.join(MARKER),
        };
        let package = PackageInfo::read(&marker)?;
        if package.kind != self.package.kind {
            return Err(UpdateError::Format(format!(
                "the new package is for {}, not {}",
                package.kind, self.package.kind
            )));
        }
        write_json(
            &staging.join(READY),
            &Ready {
                version: release.version,
            },
        )
    }

    /// The version prepared in the staging folder, if it is newer than `current` and its files
    /// are there (a swap moves them out).
    pub fn prepared(&self, current: Version) -> Option<Version> {
        let ready: Ready = fs::read_to_string(self.staging().join(READY))
            .ok()
            .and_then(|json| serde_json::from_str(&json).ok())?;
        let new = self.new_dir();
        let complete = match self.is_macos() {
            true => new_bundle(&new).is_ok(),
            false => new.join(MARKER).is_file(),
        };
        (ready.version > current && complete).then_some(ready.version)
    }

    /// Clears the staging folder at start, keeping an update prepared for a version newer than
    /// `current`. Returns why the last update failed, if it did: the app reports it. What cannot
    /// be removed yet (the browser host may still run from an `old-*` folder) is left for the
    /// next start.
    pub fn clean_up(&self, current: Version) -> Option<String> {
        let staging = self.staging();
        let failed = fs::read_to_string(staging.join(FAILED)).ok();
        let keep = failed.is_none() && self.prepared(current).is_some();
        let Ok(entries) = fs::read_dir(&staging) else {
            return failed;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            if keep && (name == NEW || name == READY) {
                continue;
            }
            remove_what_can_go(&entry.path());
        }
        let _ = fs::remove_dir(&staging);
        failed
    }
}

#[derive(Serialize, Deserialize)]
struct Ready {
    version: Version,
}

/// The app bundle in an unpacked macOS package.
pub(crate) fn new_bundle(new: &Path) -> Result<PathBuf, UpdateError> {
    fs::read_dir(new)
        .map_err(|e| io_error(new, e))?
        .flatten()
        .map(|entry| entry.path())
        .find(|path| path.extension().is_some_and(|ext| ext == "app") && path.is_dir())
        .ok_or_else(|| UpdateError::Format("the disk image has no app".into()))
}

pub(crate) fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(20)))
        .timeout_recv_response(Some(Duration::from_secs(60)))
        .timeout_per_read(Some(Duration::from_secs(30)))
        .user_agent(USER_AGENT)
        .build()
        .into()
}

/// The checksum `SHA256SUMS.txt` gives for `name` (`<hex>  <name>` lines).
fn checksum_of(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (digest, file) = line.trim().split_once(char::is_whitespace)?;
        // `*name` marks binary mode in sha256sum's output.
        let file = file.trim_start().trim_start_matches('*');
        (file == name && digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit()))
            .then(|| digest.to_ascii_lowercase())
    })
}

/// Downloads `url` into `path`, at most `size` bytes, and returns its SHA-256.
fn download_to(
    agent: &ureq::Agent,
    url: &str,
    size: u64,
    path: &Path,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<String, UpdateError> {
    let mut response = agent
        .get(url)
        .call()
        .map_err(|e| UpdateError::Http(e.to_string()))?;
    let mut reader = response.body_mut().as_reader().take(size);
    let mut file = File::create(path).map_err(|e| UpdateError::NotWritable(e.to_string()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0; 1 << 16];
    let mut done = 0;
    progress(0, size);
    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(UpdateError::Http(e.to_string())),
        };
        file.write_all(&buf[..n]).map_err(|e| io_error(path, e))?;
        hasher.update(&buf[..n]);
        done += n as u64;
        progress(done, size);
    }
    file.flush().map_err(|e| io_error(path, e))?;
    if done != size {
        return Err(UpdateError::Http(format!(
            "the download ended after {done} of {size} bytes"
        )));
    }
    Ok(hex(&hasher.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The path inside the package's top folder (`plamo-translate-desktop-…/`), from an archive
/// entry's name; `None` for the top folder itself. Names that could leave the folder are refused.
fn inner_path(name: &str) -> Result<Option<PathBuf>, UpdateError> {
    // Zips written on Windows may separate with backslashes.
    let parts: Vec<&str> = name.split(['/', '\\']).filter(|p| !p.is_empty()).collect();
    if parts.iter().any(|part| !is_plain_name(part)) {
        return Err(UpdateError::Format(format!(
            "unsafe path in the package: {name}"
        )));
    }
    Ok((parts.len() > 1).then(|| parts[1..].iter().collect()))
}

/// A single path component: not empty, `.`, `..`, a root, or a drive.
fn is_plain_name(name: &str) -> bool {
    let mut components = Path::new(name).components();
    matches!(components.next(), Some(Component::Normal(_)))
        && components.next().is_none()
        && !name.contains(['/', '\\', ':'])
}

/// Unpacks a Windows package (a zip of one folder) into `dest`.
fn unpack_zip(archive: &Path, dest: &Path) -> Result<(), UpdateError> {
    let file = File::open(archive).map_err(|e| io_error(archive, e))?;
    let mut zip =
        zip::ZipArchive::new(file).map_err(|e| UpdateError::Format(format!("zip: {e}")))?;
    fs::create_dir_all(dest).map_err(|e| io_error(dest, e))?;
    for i in 0..zip.len() {
        let mut entry = zip
            .by_index(i)
            .map_err(|e| UpdateError::Format(format!("zip: {e}")))?;
        let name = entry
            .name()
            .map_err(|e| UpdateError::Format(format!("zip: {e}")))?
            .into_owned();
        let Some(inner) = inner_path(&name)? else {
            continue;
        };
        let path = dest.join(inner);
        if entry.is_dir() {
            fs::create_dir_all(&path).map_err(|e| io_error(&path, e))?;
            continue;
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| io_error(parent, e))?;
        }
        let mut out = File::create(&path).map_err(|e| io_error(&path, e))?;
        io::copy(&mut entry, &mut out).map_err(|e| io_error(&path, e))?;
        #[cfg(unix)]
        if let Some(mode) = entry.unix_mode() {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&path, fs::Permissions::from_mode(mode & 0o777));
        }
    }
    Ok(())
}

/// Unpacks a Linux package (a gzipped tar of one folder) into `dest`. Links are refused: the
/// packages have none.
fn unpack_tar_gz(archive: &Path, dest: &Path) -> Result<(), UpdateError> {
    let file = File::open(archive).map_err(|e| io_error(archive, e))?;
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(file));
    fs::create_dir_all(dest).map_err(|e| io_error(dest, e))?;
    let entries = tar
        .entries()
        .map_err(|e| UpdateError::Format(format!("tar: {e}")))?;
    for entry in entries {
        let mut entry = entry.map_err(|e| UpdateError::Format(format!("tar: {e}")))?;
        let name = entry
            .path()
            .map_err(|e| UpdateError::Format(format!("tar: {e}")))?
            .to_string_lossy()
            .into_owned();
        let Some(inner) = inner_path(&name)? else {
            continue;
        };
        let kind = entry.header().entry_type();
        if !(kind.is_file() || kind.is_dir()) {
            return Err(UpdateError::Format(format!(
                "unexpected entry in the package: {name}"
            )));
        }
        let path = dest.join(inner);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| io_error(parent, e))?;
        }
        entry.unpack(&path).map_err(|e| io_error(&path, e))?;
    }
    Ok(())
}

/// Copies the app out of a macOS package (a disk image) into `dest`.
fn unpack_dmg(image: &Path, dest: &Path) -> Result<(), UpdateError> {
    use std::process::Command;
    let mount = dest.with_file_name("mount");
    let _ = fs::remove_dir(&mount);
    fs::create_dir_all(&mount).map_err(|e| io_error(&mount, e))?;
    fs::create_dir_all(dest).map_err(|e| io_error(dest, e))?;
    let run = |command: &mut Command| -> Result<(), UpdateError> {
        let output = command
            .output()
            .map_err(|e| UpdateError::Io(format!("cannot run {command:?}: {e}")))?;
        if output.status.success() {
            Ok(())
        } else {
            Err(UpdateError::Io(format!(
                "{command:?} failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )))
        }
    };
    run(Command::new("hdiutil")
        .args([
            "attach",
            "-nobrowse",
            "-readonly",
            "-noautoopen",
            "-mountpoint",
        ])
        .arg(&mount)
        .arg(image))?;
    let copied = new_bundle(&mount).and_then(|app| {
        let name = app.file_name().expect("a bundle has a name");
        // ditto keeps the bundle's attributes and signature.
        run(Command::new("ditto").arg(&app).arg(dest.join(name)))
    });
    let detached = run(Command::new("hdiutil")
        .args(["detach", "-quiet"])
        .arg(&mount));
    let _ = fs::remove_dir(&mount);
    copied.and(detached)
}

/// Removes `path` and what it holds, as far as it can: on Windows a file in use (the browser host
/// may still run from an `old-*` folder) cannot be removed, and the rest still goes.
fn remove_what_can_go(path: &Path) {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return;
    };
    if metadata.is_dir() {
        if let Ok(entries) = fs::read_dir(path) {
            for entry in entries.flatten() {
                remove_what_can_go(&entry.path());
            }
        }
        let _ = fs::remove_dir(path);
    } else {
        let _ = fs::remove_file(path);
    }
}

fn remove_if_exists(path: &Path) -> Result<(), UpdateError> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(io_error(path, e)),
    }
}

pub(crate) fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), UpdateError> {
    let json = serde_json::to_string_pretty(value).expect("plain data serializes");
    fs::write(path, json).map_err(|e| io_error(path, e))
}

pub(crate) fn io_error(path: &Path, e: io::Error) -> UpdateError {
    UpdateError::Io(format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn package(dir: &Path, kind: &str, entries: &[&str]) {
        fs::create_dir_all(dir).unwrap();
        write_json(
            &dir.join(MARKER),
            &PackageInfo {
                kind: kind.into(),
                entries: entries.iter().map(|e| e.to_string()).collect(),
            },
        )
        .unwrap();
    }

    #[test]
    fn finds_the_installation_of_an_executable() {
        let dir = tempfile::tempdir().unwrap();
        let app = dir
            .path()
            .join("plamo-translate-desktop-0.5.0-linux-x86_64");
        package(&app, "linux-x86_64", &["plamo-translate-desktop", MARKER]);
        let exe = app.join("plamo-translate-desktop");
        let found = Installation::of_executable(&exe, false).unwrap();
        assert_eq!(found.root, app);
        assert_eq!(found.entries, ["plamo-translate-desktop", MARKER]);
        assert_eq!(found.staging(), app.join("update"));
        assert_eq!(
            found.asset_name(Version::current()),
            format!(
                "plamo-translate-desktop-{}-linux-x86_64.tar.gz",
                Version::current()
            )
        );
        // A build from source has no marker.
        assert!(matches!(
            Installation::of_executable(&dir.path().join("target/release/app"), false),
            Err(UpdateError::NotPackaged(_))
        ));

        let bundle = dir.path().join("Applications/PLaMo.app");
        package(&bundle.join("Contents/Resources"), "macos-arm64", &[]);
        let exe = bundle.join("Contents/MacOS/plamo-translate-desktop");
        let found = Installation::of_executable(&exe, true).unwrap();
        assert_eq!(found.root, dir.path().join("Applications"));
        assert_eq!(found.entries, ["PLaMo.app"]);
        assert_eq!(found.bundle.as_deref(), Some(bundle.as_path()));
        assert!(
            found
                .asset_name(Version::current())
                .ends_with("-macos-arm64.dmg")
        );
        assert_eq!(
            found.staging(),
            dir.path().join("Applications/.plamo-translate-update")
        );
    }

    #[test]
    fn refuses_entries_that_leave_the_folder() {
        let dir = tempfile::tempdir().unwrap();
        for bad in ["..", "../x", "a/b", "C:x", "/etc", ""] {
            package(dir.path(), "linux-x86_64", &[bad]);
            assert!(
                PackageInfo::read(&dir.path().join(MARKER)).is_err(),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn reads_checksums() {
        let digest = "ab".repeat(32);
        let sums = format!(
            "{digest}  plamo-translate-desktop-0.5.1-linux-x86_64.tar.gz\n{}  *other.zip\n",
            "CD".repeat(32)
        );
        assert_eq!(
            checksum_of(&sums, "plamo-translate-desktop-0.5.1-linux-x86_64.tar.gz"),
            Some(digest)
        );
        assert_eq!(checksum_of(&sums, "other.zip"), Some("cd".repeat(32)));
        assert_eq!(checksum_of(&sums, "missing.zip"), None);
        assert_eq!(checksum_of("short  a.zip", "a.zip"), None);
    }

    #[test]
    fn strips_the_top_folder_and_refuses_unsafe_paths() {
        assert_eq!(inner_path("top/").unwrap(), None);
        assert_eq!(
            inner_path("top/a/b.dll").unwrap(),
            Some(PathBuf::from("a/b.dll"))
        );
        assert_eq!(
            inner_path("top\\browser-extension\\manifest.json").unwrap(),
            Some(PathBuf::from("browser-extension/manifest.json"))
        );
        for bad in ["top/../x", "../top/x", "top/C:/x", "top/./x"] {
            assert!(inner_path(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn unpacks_zips_and_tarballs() {
        let dir = tempfile::tempdir().unwrap();
        // A zip as Compress-Archive writes it: the folder, backslashes in names.
        let zip_path = dir.path().join("p.zip");
        {
            let mut zip = zip::ZipWriter::new(File::create(&zip_path).unwrap());
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            zip.add_directory("top/", options).unwrap();
            zip.start_file("top\\app.exe", options).unwrap();
            zip.write_all(b"exe").unwrap();
            zip.start_file("top\\browser-extension\\manifest.json", options)
                .unwrap();
            zip.write_all(b"{}").unwrap();
            zip.finish().unwrap();
        }
        let out = dir.path().join("zip-out");
        unpack_zip(&zip_path, &out).unwrap();
        assert_eq!(fs::read(out.join("app.exe")).unwrap(), b"exe");
        assert_eq!(
            fs::read(out.join("browser-extension/manifest.json")).unwrap(),
            b"{}"
        );

        let tar_path = dir.path().join("p.tar.gz");
        {
            let gz = flate2::write::GzEncoder::new(
                File::create(&tar_path).unwrap(),
                flate2::Compression::fast(),
            );
            let mut tar = tar::Builder::new(gz);
            let mut header = tar::Header::new_gnu();
            header.set_size(3);
            header.set_mode(0o755);
            header.set_cksum();
            tar.append_data(&mut header, "top/app", &b"bin"[..])
                .unwrap();
            tar.into_inner().unwrap().finish().unwrap();
        }
        let out = dir.path().join("tar-out");
        unpack_tar_gz(&tar_path, &out).unwrap();
        assert_eq!(fs::read(out.join("app")).unwrap(), b"bin");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(out.join("app")).unwrap().permissions().mode();
            assert_eq!(mode & 0o111, 0o111, "executable");
        }

        // A link is refused.
        let link_path = dir.path().join("link.tar.gz");
        {
            let gz = flate2::write::GzEncoder::new(
                File::create(&link_path).unwrap(),
                flate2::Compression::fast(),
            );
            let mut tar = tar::Builder::new(gz);
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_size(0);
            tar.append_link(&mut header, "top/link", "/etc/passwd")
                .unwrap();
            tar.into_inner().unwrap().finish().unwrap();
        }
        assert!(unpack_tar_gz(&link_path, &dir.path().join("link-out")).is_err());
    }

    #[test]
    fn keeps_a_prepared_update_and_reports_a_failed_one() {
        let dir = tempfile::tempdir().unwrap();
        package(dir.path(), "linux-x86_64", &["app"]);
        let installation = Installation::of_executable(&dir.path().join("app"), false).unwrap();
        let staging = installation.staging();
        let current = Version::current();
        let newer = Version {
            patch: current.patch + 1,
            ..current
        };
        fs::create_dir_all(staging.join("old-1")).unwrap();
        write_json(&staging.join(READY), &Ready { version: newer }).unwrap();
        fs::create_dir_all(installation.new_dir()).unwrap();
        assert_eq!(installation.prepared(current), None, "no files");
        package(&installation.new_dir(), "linux-x86_64", &["app"]);
        assert_eq!(installation.prepared(current), Some(newer));
        assert_eq!(installation.clean_up(current), None);
        assert!(installation.new_dir().is_dir(), "kept");
        assert!(!staging.join("old-1").exists(), "removed");

        // Once installed, the same version is not newer: all goes.
        assert_eq!(installation.prepared(newer), None);
        assert_eq!(installation.clean_up(newer), None);
        assert!(!staging.exists());

        fs::create_dir_all(installation.new_dir()).unwrap();
        write_json(&staging.join(READY), &Ready { version: newer }).unwrap();
        fs::write(staging.join(FAILED), "could not move app").unwrap();
        assert_eq!(
            installation.clean_up(current).as_deref(),
            Some("could not move app")
        );
        assert!(!staging.exists(), "a failed update is not kept");
    }
}
