//! Installs the newest release over the installation of the given executable, as the app's
//! update button does, without the app: `cargo run -p update --example install -- <executable>`.
//! For trying updates with a local server (`PLAMO_TRANSLATE_UPDATE_URL`): the new version's
//! executable swaps itself in once this program has exited, then starts the app.

use std::path::Path;

fn main() {
    let Some(exe) = std::env::args_os().nth(1) else {
        eprintln!("usage: install <executable of an installed package>");
        std::process::exit(2);
    };
    if let Err(e) = install(Path::new(&exe)) {
        eprintln!("{e}");
        std::process::exit(1);
    }
}

fn install(exe: &Path) -> Result<(), update::UpdateError> {
    let installation = update::Installation::of_executable(
        &exe.canonicalize()
            .map_err(|e| update::UpdateError::Io(format!("{}: {e}", exe.display())))?,
        cfg!(target_os = "macos"),
    )?;
    let release = update::latest()?;
    println!(
        "installing {} ({}) into {}",
        release.version,
        installation.asset_name(release.version),
        installation.root.display()
    );
    let mut shown = 0;
    installation.prepare(&release, &mut |done, total| {
        let percent = (done * 100).checked_div(total).unwrap_or(0);
        if percent >= shown {
            println!("  {percent}% of {total} bytes");
            shown = percent + 25;
        }
    })?;
    update::apply::start(&installation)?;
    println!("prepared; the swap starts when this program exits");
    Ok(())
}
