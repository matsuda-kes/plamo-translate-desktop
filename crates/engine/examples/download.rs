//! Downloads a catalog model from the command line.
//!
//! ```text
//! cargo run -p engine --release --example download -- [--dir DIR] [--stop-after-mb N] [MODEL_ID]
//! cargo run -p engine --release --example download -- --list
//! cargo run -p engine --release --example download -- --discover
//! ```
//!
//! `--stop-after-mb` cancels once that much has been transferred, to exercise resuming.

use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, bail};
use engine::download::{DownloadError, Downloaded, download};
use engine::models::{ModelStore, default_entry, entries, find_entry};

fn main() -> anyhow::Result<()> {
    let mut dir = None;
    let mut stop_after = None;
    let mut id = None;
    let mut list = false;
    let mut discover = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--dir" => dir = Some(PathBuf::from(args.next().context("--dir needs a value")?)),
            "--stop-after-mb" => {
                let mb: u64 = args
                    .next()
                    .context("--stop-after-mb needs a value")?
                    .parse()?;
                stop_after = Some(mb << 20);
            }
            "--list" => list = true,
            "--discover" => discover = true,
            s if s.starts_with("--") => bail!("unknown option {s}"),
            _ => id = Some(arg),
        }
    }

    if discover {
        let found = engine::discovery::discover()?;
        println!("{} GGUF files on Hugging Face", found.len());
        for d in &found {
            let builtin = engine::models::BUILTIN
                .iter()
                .find(|b| b.file_name == d.file_name);
            let note = match builtin {
                Some(b) if b.sha256 == d.sha256 => "built in",
                Some(_) => "newer than built in",
                None => "new",
            };
            println!(
                "{:<40} {:>6.2} GB  {}  {}  {note}",
                d.file_name,
                d.bytes as f64 / 1e9,
                &d.revision[..8],
                d.published.as_deref().unwrap_or("-")
            );
        }
        let added = engine::models::register(found.into_iter().map(|d| d.into_entry()));
        println!("registered {added} new or updated entries");
        return Ok(());
    }

    let store = ModelStore::new(dir.unwrap_or_else(|| PathBuf::from("models")));
    if list {
        for e in entries() {
            println!(
                "{:<36} {:>6.2} GB  {:?}",
                e.id(),
                e.bytes as f64 / 1e9,
                store.state(e)
            );
        }
        return Ok(());
    }

    let entry = match id {
        Some(id) => find_entry(&id).with_context(|| format!("unknown model {id}"))?,
        None => default_entry(),
    };
    println!("{entry} -> {}", store.path(entry).display());

    let stop = AtomicBool::new(false);
    let start = std::time::Instant::now();
    let result = download(
        entry,
        &store,
        &mut |p| {
            if stop_after.is_some_and(|limit| p.downloaded >= limit) {
                stop.store(true, Ordering::Relaxed);
            }
            print!(
                "\r{:>5.1}%  {:>8.1} MB / {:.1} MB  {:>6.1} MB/s  ETA {:>5}s ",
                p.fraction() * 100.0,
                p.downloaded as f64 / 1e6,
                p.total as f64 / 1e6,
                p.bytes_per_second / 1e6,
                p.eta().map_or("-".into(), |d| d.as_secs().to_string()),
            );
            let _ = std::io::stdout().flush();
        },
        &|| stop.load(Ordering::Relaxed),
    );
    println!();
    match result {
        Ok(Downloaded::Placed(path)) => println!(
            "verified and saved {} in {:.1?}",
            path.display(),
            start.elapsed()
        ),
        Ok(Downloaded::BesideOlder) => {
            let path = store.replace_with_download(entry)?;
            println!(
                "verified and saved {} in place of an older revision in {:.1?}",
                path.display(),
                start.elapsed()
            );
        }
        Err(DownloadError::Cancelled) => println!("stopped; state: {:?}", store.state(entry)),
        Err(e) => return Err(e.into()),
    }
    Ok(())
}
