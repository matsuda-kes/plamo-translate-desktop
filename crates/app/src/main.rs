//! PLaMo Translate Desktop.
//!
//! Models are picked and downloaded in the app. A GGUF path given as the first argument or in
//! `PLAMO_MODEL` overrides the saved selection, which is handy for development.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod align;
mod assets;
mod backend;
mod browser;
mod lang;
mod models;
mod tts;
mod view;

// Shared with the browser host; re-exported so the app refers to them as its own modules.
pub(crate) use appdata::{app_log, applog, glossary, settings};

use std::path::PathBuf;

use gpui_kit::component::TitleBar;
use gpui_kit::*;

use crate::view::TranslatorView;

rust_i18n::i18n!("locales", fallback = "en");

#[cfg(target_os = "macos")]
actions!(application, [Quit]);

fn custom_model() -> Option<PathBuf> {
    std::env::args_os()
        .nth(1)
        .or_else(|| std::env::var_os("PLAMO_MODEL"))
        .map(PathBuf::from)
}

/// What a macOS application has to set up itself: GPUI adds no menu bar and no Cmd+Q.
#[cfg(target_os = "macos")]
mod macos {
    use gpui_kit::*;

    actions!(plamo_translate, [Quit]);

    pub fn init(cx: &mut App) {
        cx.on_action(|_: &Quit, cx: &mut App| cx.quit());
        cx.bind_keys([KeyBinding::new("cmd-q", Quit, None)]);
        cx.set_menus(vec![Menu {
            name: "PLaMo Translate".into(),
            items: vec![MenuItem::action("Quit PLaMo Translate", Quit)],
            disabled: false,
        }]);
    }
}

fn main() {
    // A new version, started from the staging folder to swap itself in (see `update::apply`):
    // it does only that, without a window.
    if let Some(plan) = update::apply::requested() {
        applog::init(&settings::log_path());
        match update::apply::run(&plan) {
            Ok(()) => app_log!("update to {} installed", env!("CARGO_PKG_VERSION")),
            Err(e) => app_log!("update to {} failed: {e}", env!("CARGO_PKG_VERSION")),
        }
        return;
    }
    // SAFETY: first thing in main, before any thread starts.
    unsafe {
        appdata::devices::configure_openvino();
        engine::llama::configure_metal();
    }
    let custom_model = custom_model();
    applog::init(&settings::log_path());
    // The release build has no console on Windows: a panic is recorded in the log, then handled
    // as before (the message on stderr, and the thread unwinding).
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        app_log!("panic: {info}");
        default_hook(info);
    }));
    let settings = settings::Settings::load();
    app_log!(
        "starting {} {}; models in {}",
        env!("CARGO_PKG_NAME"),
        env!("CARGO_PKG_VERSION"),
        settings.models_dir().display()
    );
    // Files found on Hugging Face by earlier runs; the Models dialog refreshes this list.
    let cached = engine::discovery::load_cache(&settings::catalog_cache_path());
    let added = engine::models::register(cached.into_iter().map(|d| d.into_entry()));
    if added > 0 {
        app_log!("catalog: {added} cached entries newer than the built-in list");
    }
    // Shared with gpui-component, so its built-in strings follow the same locale.
    rust_i18n::set_locale(settings.ui_language.locale());

    gpui_kit::application()
        .with_assets(assets::AppAssets)
        // The default on macOS keeps the process running without a window, which cannot be
        // reopened here. Windows and Linux already quit with the last window.
        .with_quit_mode(QuitMode::LastWindowClosed)
        .run(move |cx| {
            gpui_kit::init(cx);
            view::bind_keys(cx);
            #[cfg(target_os = "macos")]
            macos::init(cx);
            // A larger UI size opens a larger window too, within the screen.
            let scale = settings.ui_scale_factor();
            let screen = cx.primary_display().map(|d| d.bounds().size);
            let fit = |width: f32, height: f32| {
                let (max_width, max_height) = screen.map_or((f32::MAX, f32::MAX), |s| {
                    (f32::from(s.width) * 0.9, f32::from(s.height) * 0.9)
                });
                size(
                    px((width * scale).min(max_width)),
                    px((height * scale).min(max_height)),
                )
            };
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                    None,
                    fit(1100., 720.),
                    cx,
                ))),
                window_min_size: Some(fit(640., 400.)),
                // Matches the desktop entry name on Linux, so the window gets its icon.
                app_id: Some("plamo-translate-desktop".into()),
                ..TitleBar::window_options()
            };
            gpui_kit::open_window(options, cx, |window, cx| {
                view::apply_appearance(&settings, window, cx);
                cx.new(|cx| TranslatorView::new(custom_model, settings, window, cx))
            })
            .expect("failed to open the main window");
            cx.activate(true);
        });
}
