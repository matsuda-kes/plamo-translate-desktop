//! Prints the newest release on GitHub and whether it is newer than this build:
//! `cargo run -p update --example latest`. Set `PLAMO_TRANSLATE_UPDATE_URL` to ask another server.

fn main() {
    let current = update::Version::current();
    match update::latest() {
        Ok(release) => {
            println!(
                "newest: {} ({}), this build: {current}, newer: {}",
                release.version,
                release.page,
                release.version > current
            );
            for asset in &release.assets {
                println!("  {} ({} bytes)", asset.name, asset.size);
            }
        }
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}
