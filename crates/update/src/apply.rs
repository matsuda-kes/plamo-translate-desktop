//! Swapping a prepared update in.
//!
//! The running app cannot replace its own files on Windows, so it writes a [`Plan`] and starts the
//! new version's executable from the staging folder with `--apply-update <plan>` ([`start`]),
//! then quits. That process waits until the app has exited (its standard input, a pipe from the
//! app, closes), moves the installation's entries into an `old-*` folder and the new ones into
//! place, puts everything back if a step fails, and starts the app ([`requested`], [`run`]).
//! Renaming works on files in use on Windows too, so a browser host still running from the old
//! files does not stop it; the next start removes the `old-*` folder once nothing uses it.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, SystemTime};

use crate::unix_seconds;

use serde::{Deserialize, Serialize};

use crate::UpdateError;
use crate::install::{
    FAILED, Installation, MARKER, PackageInfo, READY, io_error, new_bundle, write_json,
};

/// The argument that makes the app carry out a plan instead of starting.
pub const APPLY_ARG: &str = "--apply-update";

const PLAN: &str = "plan.json";

/// How long the new version waits for the app to exit. After that it swaps nothing: renaming the
/// files of a running app would work, and leave two copies of it running.
const PARENT_WAIT: Duration = Duration::from_secs(60);

/// What the new version's executable does once the app has exited. One version writes it and
/// the next reads it, so fields are only ever added.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    /// The installation's entries, moved into `old` (those that exist).
    pub remove: Vec<PathBuf>,
    /// The new entries, and where they go.
    pub place: Vec<(PathBuf, PathBuf)>,
    pub old: PathBuf,
    /// The program started afterwards, and its arguments: the app, new or (after a failure) old.
    pub launch: Vec<PathBuf>,
    /// Where a failure is recorded, for the app to report.
    pub failed: PathBuf,
    /// Removed after a successful swap: the record that an update is prepared.
    #[serde(default)]
    pub done: Vec<PathBuf>,
}

impl Installation {
    /// The plan that swaps the prepared update in, and the new version's executable, which
    /// carries it out.
    pub fn plan(&self) -> Result<(Plan, PathBuf), UpdateError> {
        let staging = self.staging();
        let new = self.new_dir();
        let old = staging.join(format!("old-{}", unix_seconds(SystemTime::now())));
        let executable = self
            .executable
            .file_name()
            .ok_or_else(|| UpdateError::NotPackaged(self.executable.display().to_string()))?;
        let (remove, place, launch, helper) = match &self.bundle {
            Some(bundle) => {
                // The app keeps its name, even if the user renamed it.
                let app = new_bundle(&new)?;
                let helper = app.join("Contents/MacOS").join(executable);
                (
                    vec![bundle.clone()],
                    vec![(app, bundle.clone())],
                    vec![PathBuf::from("open"), bundle.clone()],
                    helper,
                )
            }
            None => {
                let package = PackageInfo::read(&new.join(MARKER))?;
                // The old package's entries go, also those the new one does not have.
                let mut remove: Vec<PathBuf> = Vec::new();
                for entry in self.entries.iter().chain(&package.entries) {
                    let path = self.root.join(entry);
                    if path != staging && !remove.contains(&path) {
                        remove.push(path);
                    }
                }
                let place = package
                    .entries
                    .iter()
                    .map(|entry| (new.join(entry), self.root.join(entry)))
                    .collect();
                (
                    remove,
                    place,
                    vec![self.root.join(executable)],
                    new.join(executable),
                )
            }
        };
        if !helper.is_file() {
            return Err(UpdateError::Format(format!(
                "the new package has no {}",
                helper.display()
            )));
        }
        let plan = Plan {
            remove,
            place,
            old,
            launch,
            failed: staging.join(FAILED),
            done: vec![staging.join(READY)],
        };
        Ok((plan, helper))
    }
}

/// Starts the prepared version's executable, which swaps it in once this process has exited.
/// The app quits right after.
pub fn start(installation: &Installation) -> Result<(), UpdateError> {
    let (plan, helper) = installation.plan()?;
    let path = installation.staging().join(PLAN);
    write_json(&path, &plan)?;
    let mut child = Command::new(&helper)
        .arg(APPLY_ARG)
        .arg(&path)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| UpdateError::Io(format!("cannot start {}: {e}", helper.display())))?;
    // The new version waits until its input closes, which happens when this process exits:
    // keep the pipe open until then.
    std::mem::forget(child.stdin.take());
    Ok(())
}

/// The plan this process was started to carry out (`--apply-update <plan>`), if it was: the app
/// asks first thing, and then only calls [`run`].
pub fn requested() -> Option<PathBuf> {
    let mut args = std::env::args_os().skip(1);
    (args.next()? == APPLY_ARG).then(|| PathBuf::from(args.next().unwrap_or_default()))
}

/// Carries out the plan at `path` once the app has exited, then starts the app.
pub fn run(path: &Path) -> Result<(), UpdateError> {
    let plan = match read_plan(path) {
        Ok(plan) => plan,
        Err(e) => {
            // Nothing can be swapped: the app starts as it was, and reports the failure.
            let _ = fs::write(path.with_file_name(FAILED), e.to_string());
            if wait_for_parent(PARENT_WAIT) {
                launch(&fallback_launch(path));
            }
            return Err(e);
        }
    };
    let exited = wait_for_parent(PARENT_WAIT);
    let result = if exited {
        swap(&plan)
    } else {
        Err(UpdateError::Io(
            "the app did not exit within a minute".into(),
        ))
    };
    match &result {
        Ok(()) => {
            for path in &plan.done {
                let _ = fs::remove_file(path);
            }
        }
        Err(e) => {
            let _ = fs::write(&plan.failed, e.to_string());
        }
    }
    // The app, new or (after a failure) old; not a second copy while the old one still runs.
    if exited {
        launch(&plan.launch);
    }
    result
}

fn read_plan(path: &Path) -> Result<Plan, UpdateError> {
    let json = fs::read_to_string(path).map_err(|e| io_error(path, e))?;
    serde_json::from_str(&json).map_err(|e| UpdateError::Format(format!("plan: {e}")))
}

/// What to start when the plan cannot be read: the executable of the same name in the folder
/// holding the staging folder (Windows and Linux). On macOS the app is not known, and nothing is.
fn fallback_launch(plan: &Path) -> Vec<PathBuf> {
    let root = plan.parent().and_then(Path::parent);
    let name = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.file_name().map(ToOwned::to_owned));
    match (root, name) {
        (Some(root), Some(name)) if root.join(&name).is_file() => vec![root.join(name)],
        _ => Vec::new(),
    }
}

fn launch(command: &[PathBuf]) {
    if let Some((program, args)) = command.split_first() {
        let _ = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
    }
}

/// Waits until standard input closes (the app that started this process has exited), at most
/// `limit`. Returns whether it closed.
fn wait_for_parent(limit: Duration) -> bool {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = std::io::stdin().read_to_end(&mut Vec::new());
        let _ = tx.send(());
    });
    rx.recv_timeout(limit).is_ok()
}

/// Moves the old entries out and the new ones in; after a failure, puts back what was moved.
pub(crate) fn swap(plan: &Plan) -> Result<(), UpdateError> {
    fs::create_dir_all(&plan.old).map_err(|e| io_error(&plan.old, e))?;
    let mut removed: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut placed: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut steps = || -> Result<(), UpdateError> {
        for path in &plan.remove {
            if fs::symlink_metadata(path).is_err() {
                continue;
            }
            let name = path
                .file_name()
                .ok_or_else(|| UpdateError::Format(format!("plan: {}", path.display())))?;
            let to = plan.old.join(name);
            rename(path, &to)?;
            removed.push((path.clone(), to));
        }
        for (from, to) in &plan.place {
            rename(from, to)?;
            placed.push((from.clone(), to.clone()));
        }
        Ok(())
    };
    let result = steps();
    if result.is_err() {
        for (from, to) in placed.iter().rev() {
            let _ = rename(to, from);
        }
        for (original, moved) in removed.iter().rev() {
            let _ = rename(moved, original);
        }
    }
    result
}

/// Renames, retrying for a few seconds: on Windows a virus scanner or the indexer can hold a
/// file briefly.
fn rename(from: &Path, to: &Path) -> Result<(), UpdateError> {
    const TRIES: u32 = 20;
    let mut tries = 0;
    loop {
        match fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(e)
                if tries < TRIES
                    && !matches!(
                        e.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::AlreadyExists
                    ) =>
            {
                tries += 1;
                std::thread::sleep(Duration::from_millis(250));
            }
            Err(e) => {
                return Err(UpdateError::Io(format!(
                    "cannot move {} to {}: {e}",
                    from.display(),
                    to.display()
                )));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn marker(dir: &Path, entries: &[&str]) {
        write_json(
            &dir.join(MARKER),
            &PackageInfo {
                kind: "linux-x86_64".into(),
                entries: entries.iter().map(|e| e.to_string()).collect(),
            },
        )
        .unwrap();
    }

    /// An installation with an app, a library the next version drops, an extension folder, and
    /// a model the user keeps in the folder; and a prepared update.
    fn installation() -> (tempfile::TempDir, Installation) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("app");
        write(&root.join("app"), "old app");
        write(&root.join("libgone.so"), "old lib");
        write(&root.join("ext/manifest.json"), "old ext");
        write(&root.join("my-model.gguf"), "user's");
        marker(&root, &["app", "libgone.so", "ext", MARKER]);
        let installation = Installation::of_executable(&root.join("app"), false).unwrap();
        let new = installation.new_dir();
        write(&new.join("app"), "new app");
        write(&new.join("ext/manifest.json"), "new ext");
        write(&new.join("libnew.so"), "new lib");
        marker(&new, &["app", "ext", "libnew.so", MARKER]);
        (dir, installation)
    }

    #[test]
    fn swaps_the_package_entries_only() {
        let (_dir, installation) = installation();
        let root = &installation.root;
        let (plan, helper) = installation.plan().unwrap();
        assert_eq!(helper, installation.new_dir().join("app"));
        assert_eq!(plan.launch, [root.join("app")]);
        swap(&plan).unwrap();
        let read = |name: &str| fs::read_to_string(root.join(name)).ok();
        assert_eq!(read("app").as_deref(), Some("new app"));
        assert_eq!(read("ext/manifest.json").as_deref(), Some("new ext"));
        assert_eq!(read("libnew.so").as_deref(), Some("new lib"));
        assert_eq!(read("libgone.so"), None, "dropped by the new package");
        assert_eq!(read("my-model.gguf").as_deref(), Some("user's"), "kept");
        assert_eq!(
            fs::read_to_string(plan.old.join("app")).unwrap(),
            "old app",
            "moved aside"
        );
        // The marker now describes the new package.
        let again = Installation::of_executable(&root.join("app"), false).unwrap();
        assert_eq!(again.entries, ["app", "ext", "libnew.so", MARKER]);
    }

    #[test]
    fn puts_everything_back_after_a_failure() {
        let (_dir, installation) = installation();
        let root = &installation.root;
        let (mut plan, _) = installation.plan().unwrap();
        // A new entry that is not there fails the swap after the others moved.
        plan.place
            .push((installation.new_dir().join("missing"), root.join("missing")));
        assert!(swap(&plan).is_err());
        let read = |name: &str| fs::read_to_string(root.join(name)).ok();
        assert_eq!(read("app").as_deref(), Some("old app"));
        assert_eq!(read("libgone.so").as_deref(), Some("old lib"));
        assert_eq!(read("ext/manifest.json").as_deref(), Some("old ext"));
        assert_eq!(read("libnew.so"), None);
        assert_eq!(
            fs::read_to_string(installation.new_dir().join("app")).unwrap(),
            "new app",
            "back in the staging folder"
        );
    }

    #[test]
    fn plans_the_macos_app_as_one_entry() {
        let dir = tempfile::tempdir().unwrap();
        let apps = dir.path().join("Applications");
        let bundle = apps.join("My PLaMo.app");
        let exe = bundle.join("Contents/MacOS/plamo-translate-desktop");
        write(&exe, "old");
        fs::create_dir_all(bundle.join("Contents/Resources")).unwrap();
        write_json(
            &bundle.join("Contents/Resources").join(MARKER),
            &PackageInfo {
                kind: "macos-arm64".into(),
                entries: Vec::new(),
            },
        )
        .unwrap();
        let installation = Installation::of_executable(&exe, true).unwrap();
        let new_app = installation.new_dir().join("PLaMo Translate Desktop.app");
        write(
            &new_app.join("Contents/MacOS/plamo-translate-desktop"),
            "new",
        );
        let (plan, helper) = installation.plan().unwrap();
        assert_eq!(plan.remove, std::slice::from_ref(&bundle));
        assert_eq!(plan.place, [(new_app.clone(), bundle.clone())]);
        assert_eq!(plan.launch, [PathBuf::from("open"), bundle.clone()]);
        assert_eq!(
            helper,
            new_app.join("Contents/MacOS/plamo-translate-desktop")
        );
        swap(&plan).unwrap();
        assert_eq!(fs::read_to_string(&exe).unwrap(), "new");
    }
}
