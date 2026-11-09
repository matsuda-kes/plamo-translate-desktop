//! The model manager: catalog, downloads and the license gate.

use std::collections::HashMap;
use std::path::PathBuf;

use engine::discovery;
use engine::download::{DownloadEvent, Downloads, Progress};
use engine::models::{self as catalog, CatalogEntry, LocalState, ModelStore};
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::progress::Progress as ProgressBar;
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use rust_i18n::t;

use crate::view::scaled;

const LICENSE_URL_EN: &str = "https://plamo.preferredai.jp/info/plamo-community-license-en";
const LICENSE_URL_JA: &str = "https://plamo.preferredai.jp/info/plamo-community-license-ja";
/// Where the quality figures come from.
const QUALITY_SOURCE_URL: &str = "https://huggingface.co/pfnet/plamo-3-translate-2b-GGUF";

/// The PLaMo Community License in the UI language.
pub fn license_url() -> &'static str {
    if rust_i18n::locale().starts_with("ja") {
        LICENSE_URL_JA
    } else {
        LICENSE_URL_EN
    }
}

pub enum ModelsEvent {
    /// The user picked a downloaded model, or the download they started first finished.
    Use(&'static CatalogEntry),
    /// Release the active model.
    Unload,
    /// Delete the active model. The owner unloads it first, then calls [`ModelsPanel::delete`].
    DeleteActive(&'static CatalogEntry),
    /// The newest revision of the active model is downloaded beside the file in use. The owner
    /// unloads the model, calls [`ModelsPanel::replace`], then loads it again.
    ReplaceActive(&'static CatalogEntry),
    /// Show the license agreement (downloads stay disabled until it is accepted).
    ReviewLicense,
    /// Entries were added or updated from Hugging Face.
    CatalogUpdated,
}

/// Result of checking Hugging Face for newly published files.
enum CatalogStatus {
    Checking,
    UpToDate,
    Updated(usize),
    Failed(String),
}

pub struct ModelsPanel {
    downloads: Downloads,
    progress: HashMap<&'static str, Progress>,
    errors: HashMap<&'static str, String>,
    active: Option<&'static str>,
    recommended: &'static CatalogEntry,
    license_accepted: bool,
    /// Whether the first download to finish becomes the active model: set when a download
    /// starts with no model active, cleared when one becomes active or no download is left.
    use_when_done: bool,
    catalog_status: CatalogStatus,
    catalog_cache: PathBuf,
    _events: Task<()>,
    _refresh: Task<()>,
}

impl EventEmitter<ModelsEvent> for ModelsPanel {}

impl ModelsPanel {
    pub fn new(
        store: ModelStore,
        recommended: &'static CatalogEntry,
        license_accepted: bool,
        catalog_cache: PathBuf,
        cx: &mut Context<Self>,
    ) -> Self {
        let (tx, rx) = async_channel::unbounded();
        let downloads = Downloads::new(store, move |event| {
            let _ = tx.send_blocking(event);
        });
        let events = cx.spawn(async move |this, cx| {
            while let Ok(event) = rx.recv().await {
                if this
                    .update(cx, |this, cx| this.on_download_event(event, cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        let mut this = Self {
            downloads,
            progress: HashMap::new(),
            errors: HashMap::new(),
            active: None,
            recommended,
            license_accepted,
            use_when_done: false,
            catalog_status: CatalogStatus::Checking,
            catalog_cache,
            _events: events,
            _refresh: Task::ready(()),
        };
        this.refresh_catalog(cx);
        // Partial downloads of revisions no longer in the catalog can never be resumed (PFN
        // stops serving replaced files), and can be gigabytes.
        let store = this.store().clone();
        cx.background_spawn(async move {
            for entry in catalog::entries() {
                let freed = store.remove_stale_partials(entry);
                if freed > 0 {
                    crate::app_log!(
                        "removed a partial download of an older revision of {} ({freed} bytes)",
                        entry.id()
                    );
                }
            }
        })
        .detach();
        this
    }

    /// Looks for newly published files on Hugging Face in the background.
    fn refresh_catalog(&mut self, cx: &mut Context<Self>) {
        self.catalog_status = CatalogStatus::Checking;
        let cache = self.catalog_cache.clone();
        self._refresh = cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    let found = discovery::discover()?;
                    if let Err(e) = discovery::save_cache(&cache, &found) {
                        crate::app_log!("cannot save the catalog cache: {e}");
                    }
                    Ok::<_, discovery::DiscoveryError>(found)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.catalog_status = match result {
                    Ok(found) => {
                        let total = found.len();
                        let changed = catalog::register(found.into_iter().map(|d| d.into_entry()));
                        crate::app_log!(
                            "catalog: {total} files on Hugging Face, {changed} new or updated"
                        );
                        if changed > 0 {
                            cx.emit(ModelsEvent::CatalogUpdated);
                            CatalogStatus::Updated(changed)
                        } else {
                            CatalogStatus::UpToDate
                        }
                    }
                    Err(e) => {
                        crate::app_log!("catalog check failed: {e}");
                        CatalogStatus::Failed(e.to_string())
                    }
                };
                cx.notify();
            });
        });
    }

    pub fn store(&self) -> &ModelStore {
        self.downloads.store()
    }

    pub fn set_license_accepted(&mut self, accepted: bool, cx: &mut Context<Self>) {
        self.license_accepted = accepted;
        cx.notify();
    }

    pub fn set_recommended(&mut self, entry: &'static CatalogEntry, cx: &mut Context<Self>) {
        self.recommended = entry;
        cx.notify();
    }

    pub fn set_active(&mut self, entry: Option<&'static CatalogEntry>, cx: &mut Context<Self>) {
        self.active = entry.map(CatalogEntry::id);
        // A model chosen meanwhile is not replaced by a download finishing later.
        if self.active.is_some() {
            self.use_when_done = false;
        }
        cx.notify();
    }

    fn on_download_event(&mut self, event: DownloadEvent, cx: &mut Context<Self>) {
        match event {
            DownloadEvent::Progress { entry, progress } => {
                self.progress.insert(entry.id(), progress);
            }
            DownloadEvent::Finished { entry, path } => {
                crate::app_log!("download finished: {}", path.display());
                self.progress.remove(entry.id());
                if self.use_when_done && self.active.is_none() {
                    self.use_when_done = false;
                    cx.emit(ModelsEvent::Use(entry));
                }
            }
            DownloadEvent::Verified { entry } => {
                crate::app_log!("downloaded beside an older revision: {}", entry.id());
                self.progress.remove(entry.id());
                if self.active == Some(entry.id()) {
                    cx.emit(ModelsEvent::ReplaceActive(entry));
                } else if self.replace(entry, cx) && self.use_when_done && self.active.is_none() {
                    self.use_when_done = false;
                    cx.emit(ModelsEvent::Use(entry));
                }
            }
            DownloadEvent::Cancelled { entry } => {
                crate::app_log!("download cancelled: {}", entry.id());
                self.progress.remove(entry.id());
                self.use_when_done &= !self.progress.is_empty();
            }
            DownloadEvent::Failed { entry, error } => {
                crate::app_log!("download failed: {}: {error}", entry.id());
                self.progress.remove(entry.id());
                self.errors.insert(entry.id(), error);
                self.use_when_done &= !self.progress.is_empty();
            }
        }
        cx.notify();
    }

    fn start(&mut self, entry: &'static CatalogEntry, cx: &mut Context<Self>) {
        if !self.license_accepted {
            return;
        }
        self.errors.remove(entry.id());
        crate::app_log!(
            "download started: {} (local state {:?})",
            entry.id(),
            self.store().state(entry)
        );
        let downloaded = self.store().state(entry).partial().unwrap_or(0);
        self.progress.insert(
            entry.id(),
            Progress {
                downloaded,
                total: entry.bytes,
                bytes_per_second: 0.0,
                average_bytes_per_second: 0.0,
            },
        );
        if self.active.is_none() {
            self.use_when_done = true;
        }
        self.downloads.start(entry);
        cx.notify();
    }

    fn cancel(&mut self, entry: &'static CatalogEntry, cx: &mut Context<Self>) {
        self.downloads.cancel(entry);
        cx.notify();
    }

    fn confirm_delete(
        &mut self,
        entry: &'static CatalogEntry,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let panel = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let panel = panel.clone();
            alert
                .title(t!("models.delete_title").into_owned())
                .description(
                    t!(
                        "models.delete_body",
                        model = entry.to_string(),
                        size = format_bytes(entry.bytes)
                    )
                    .into_owned(),
                )
                .show_cancel(true)
                .ok_text(t!("models.delete").into_owned())
                .ok_variant(ButtonVariant::Danger)
                .cancel_text(t!("models.cancel").into_owned())
                .on_ok(move |_, _, cx| {
                    let _ = panel.update(cx, |this, cx| {
                        if this.active == Some(entry.id()) {
                            cx.emit(ModelsEvent::DeleteActive(entry));
                        } else {
                            this.delete(entry, "confirmed in the Models dialog", cx);
                        }
                    });
                    true
                })
        });
    }

    /// Deletes the files of a model that is not loaded. Returns whether they are gone; a
    /// failure shows in the model's row.
    pub fn delete(
        &mut self,
        entry: &'static CatalogEntry,
        reason: &str,
        cx: &mut Context<Self>,
    ) -> bool {
        crate::app_log!(
            "deleting model {} ({reason}): {}",
            entry.id(),
            self.store().path(entry).display()
        );
        let deleted = match self.store().delete(entry) {
            Ok(()) => {
                self.errors.remove(entry.id());
                true
            }
            Err(e) => {
                crate::app_log!("deleting {} failed: {e}", entry.id());
                self.errors.insert(entry.id(), e.to_string());
                false
            }
        };
        cx.notify();
        deleted
    }

    /// Puts the newest revision of `entry`, downloaded beside the older file, in its place. The
    /// model must not be loaded. Returns whether it is in place; a failure shows in the row (the
    /// browser host may still hold the file on Windows), and updating again retries.
    pub fn replace(&mut self, entry: &'static CatalogEntry, cx: &mut Context<Self>) -> bool {
        let replaced = match self.store().replace_with_download(entry) {
            Ok(path) => {
                crate::app_log!("replaced an older revision: {}", path.display());
                self.errors.remove(entry.id());
                true
            }
            Err(e) => {
                crate::app_log!("replacing {} failed: {e}", entry.id());
                self.errors.insert(entry.id(), e.to_string());
                false
            }
        };
        cx.notify();
        replaced
    }

    fn render_row(&self, entry: &'static CatalogEntry, cx: &mut Context<Self>) -> AnyElement {
        let (danger, warning, secondary, muted) = {
            let theme = cx.theme();
            (
                theme.danger,
                theme.warning,
                theme.secondary,
                theme.muted_foreground,
            )
        };
        let id = entry.id();
        let state = self.store().state(entry);
        let progress = self.progress.get(id).copied();
        let error = self.errors.get(id).cloned();
        let is_active = self.active == Some(id);
        let can_download = self.license_accepted;

        let status: AnyElement = if let Some(p) = progress {
            v_flex()
                .gap_1()
                .child(
                    ProgressBar::new(SharedString::from(format!("progress-{id}")))
                        .value(p.fraction() * 100.0)
                        .w_full(),
                )
                .child(div().text_xs().child(progress_text(&p)))
                .into_any_element()
        } else if let Some(error) = &error {
            div()
                .text_color(danger)
                .child(t!("models.failed", message = error).into_owned())
                .into_any_element()
        } else {
            let present = || {
                if is_active {
                    Tag::primary()
                        .small()
                        .child(t!("models.in_use").into_owned())
                        .into_any_element()
                } else {
                    div()
                        .child(t!("models.downloaded").into_owned())
                        .into_any_element()
                }
            };
            let paused = |n: u64| {
                t!(
                    "models.paused",
                    percent = format!("{:.0}", n as f64 / entry.bytes as f64 * 100.0)
                )
                .into_owned()
            };
            match state {
                LocalState::Downloaded => present(),
                LocalState::Outdated { partial } => {
                    let mut newer = match entry.published_date() {
                        Some(date) => t!("models.newer_dated", date = date),
                        None => t!("models.newer"),
                    }
                    .into_owned();
                    if let Some(n) = partial {
                        newer = format!("{newer} · {}", paused(n));
                    }
                    v_flex()
                        .gap_0p5()
                        .items_start()
                        .child(present())
                        .child(div().text_xs().text_color(muted).child(newer))
                        .into_any_element()
                }
                LocalState::Partial(n) => div().child(paused(n)).into_any_element(),
                LocalState::Missing => div().into_any_element(),
            }
        };

        let button = |name: &str| Button::new(SharedString::from(format!("{name}-{id}"))).xsmall();
        // A fixed-width column keeps Delete in the same place whichever action precedes it.
        let actions = h_flex()
            .w(scaled(210.))
            .flex_shrink_0()
            .gap_1()
            .justify_end()
            .map(|this| {
                if progress.is_some() {
                    this.child(
                        button("cancel")
                            .label(t!("models.cancel").into_owned())
                            .on_click(cx.listener(move |this, _, _, cx| this.cancel(entry, cx))),
                    )
                } else {
                    match state {
                        LocalState::Downloaded | LocalState::Outdated { .. } => this
                            .when(matches!(state, LocalState::Outdated { .. }), |this| {
                                this.child(
                                    button("update")
                                        .label(
                                            if error.is_some() {
                                                t!("models.retry")
                                            } else {
                                                t!("models.update")
                                            }
                                            .into_owned(),
                                        )
                                        // What the newer version changed.
                                        .when_some(entry.change, |this, change| {
                                            this.tooltip(change)
                                        })
                                        .disabled(!can_download)
                                        .on_click(
                                            cx.listener(move |this, _, _, cx| {
                                                this.start(entry, cx)
                                            }),
                                        ),
                                )
                            })
                            .map(|this| {
                                if is_active {
                                    this.child(
                                        button("unload")
                                            .label(t!("models.unload").into_owned())
                                            .on_click(cx.listener(|_, _, _, cx| {
                                                cx.emit(ModelsEvent::Unload)
                                            })),
                                    )
                                } else {
                                    this.child(
                                        button("use")
                                            .primary()
                                            .label(t!("models.use").into_owned())
                                            .on_click(cx.listener(move |_, _, _, cx| {
                                                cx.emit(ModelsEvent::Use(entry))
                                            })),
                                    )
                                }
                            })
                            .child(
                                button("delete")
                                    .ghost()
                                    .label(t!("models.delete").into_owned())
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.confirm_delete(entry, window, cx)
                                    })),
                            ),
                        LocalState::Partial(_) => this
                            .child(
                                button("resume")
                                    .primary()
                                    .label(t!("models.resume").into_owned())
                                    .disabled(!can_download)
                                    .on_click(
                                        cx.listener(move |this, _, _, cx| this.start(entry, cx)),
                                    ),
                            )
                            .child(
                                button("delete")
                                    .ghost()
                                    .label(t!("models.delete").into_owned())
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.confirm_delete(entry, window, cx)
                                    })),
                            ),
                        LocalState::Missing => this.child(
                            button("download")
                                .label(
                                    if error.is_some() {
                                        t!("models.retry")
                                    } else {
                                        t!("models.download")
                                    }
                                    .into_owned(),
                                )
                                .disabled(!can_download)
                                .on_click(cx.listener(move |this, _, _, cx| this.start(entry, cx))),
                        ),
                    }
                }
            });

        h_flex()
            .id(SharedString::from(format!("row-{id}")))
            .gap_3()
            .px_2()
            .py_1p5()
            .rounded_md()
            .when(is_active, |this| this.bg(secondary))
            .child(
                v_flex()
                    .w(scaled(230.))
                    .flex_shrink_0()
                    .child(
                        h_flex()
                            .gap_2()
                            .child(div().font_weight(FontWeight::MEDIUM).child(entry.quant))
                            .when(std::ptr::eq(entry, self.recommended), |this| {
                                this.child(
                                    Tag::secondary()
                                        .small()
                                        .child(t!("models.recommended").into_owned()),
                                )
                            }),
                    )
                    .when_some(quant_note(entry.quant), |this, note| {
                        this.child(div().text_xs().text_color(muted).child(note))
                    }),
            )
            .child(
                div()
                    .w(scaled(70.))
                    .flex_shrink_0()
                    .text_color(muted)
                    .child(format_bytes(entry.bytes)),
            )
            .child(quality_cell(entry, muted, warning, danger))
            .child(div().flex_1().min_w_0().text_sm().child(status))
            .child(actions)
            .into_any_element()
    }
}

impl Render for ModelsPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (border, muted, divider) = {
            let theme = cx.theme();
            (theme.warning, theme.muted_foreground, theme.border)
        };
        // Rows grouped by model size, each group under a header.
        let mut rows: Vec<AnyElement> = Vec::new();
        let mut current_base = String::new();
        for entry in catalog::entries() {
            let base = entry.base_model();
            if base != current_base {
                rows.push(size_header(entry, muted, divider));
                current_base = base;
            }
            rows.push(self.render_row(entry, cx));
        }
        let catalog_text = match &self.catalog_status {
            CatalogStatus::Checking => t!("models.catalog_checking").into_owned(),
            CatalogStatus::UpToDate => t!("models.catalog_up_to_date").into_owned(),
            CatalogStatus::Updated(n) => t!("models.catalog_updated", count = n).into_owned(),
            CatalogStatus::Failed(e) => t!("models.catalog_failed", message = e).into_owned(),
        };
        let license_url = license_url();
        let models_dir = self.store().dir().to_path_buf();

        v_flex()
            .gap_3()
            .text_sm()
            .map(|this| {
                if self.license_accepted {
                    this.child(
                        h_flex()
                            .gap_2()
                            .text_xs()
                            .text_color(muted)
                            .child("Built with PLaMo")
                            .child("·")
                            .child(
                                Button::new("license-link")
                                    .link()
                                    .xsmall()
                                    .label(t!("models.license_name").into_owned())
                                    .on_click(move |_, _, cx| cx.open_url(license_url)),
                            ),
                    )
                } else {
                    this.child(
                        h_flex()
                            .gap_3()
                            .p_2()
                            .rounded_md()
                            .border_1()
                            .border_color(border)
                            .child(
                                div()
                                    .flex_1()
                                    .child(t!("models.license_required").into_owned()),
                            )
                            .child(
                                Button::new("review-license")
                                    .primary()
                                    .xsmall()
                                    .label(t!("models.review_license").into_owned())
                                    .on_click(cx.listener(|_, _, _, cx| {
                                        cx.emit(ModelsEvent::ReviewLicense)
                                    })),
                            ),
                    )
                }
            })
            .child(
                h_flex()
                    .gap_2()
                    .text_xs()
                    .text_color(muted)
                    .child(t!("models.quality_legend").into_owned())
                    .child(
                        Button::new("quality-source")
                            .link()
                            .xsmall()
                            .label(t!("models.quality_source").into_owned())
                            .on_click(|_, _, cx| cx.open_url(QUALITY_SOURCE_URL)),
                    ),
            )
            .child(
                div()
                    .id("model-list")
                    .max_h(scaled(440.))
                    .overflow_y_scroll()
                    .child(v_flex().gap_0p5().children(rows)),
            )
            .child(
                h_flex()
                    .gap_2()
                    .text_xs()
                    .text_color(muted)
                    .child(
                        t!("models.folder", path = models_dir.display().to_string()).into_owned(),
                    )
                    .child(
                        Button::new("open-folder")
                            .ghost()
                            .xsmall()
                            .label(t!("models.open_folder").into_owned())
                            .on_click(move |_, _, cx| {
                                let _ = std::fs::create_dir_all(&models_dir);
                                cx.reveal_path(&models_dir);
                            }),
                    )
                    .child(div().flex_1())
                    .child(catalog_text)
                    .when(
                        !matches!(self.catalog_status, CatalogStatus::Checking),
                        |this| {
                            this.child(
                                Button::new("refresh-catalog")
                                    .ghost()
                                    .xsmall()
                                    .label(t!("models.catalog_refresh").into_owned())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.refresh_catalog(cx);
                                        cx.notify();
                                    })),
                            )
                        },
                    ),
            )
    }
}

/// Header above the files of one model size: description and the unquantized quality.
fn size_header(entry: &CatalogEntry, muted: Hsla, divider: Hsla) -> AnyElement {
    let description = match entry.size {
        "0.8b" => t!("models.size_08b"),
        "2b" => t!("models.size_2b"),
        "8b" => t!("models.size_8b"),
        "31b" => t!("models.size_31b"),
        _ => t!("models.size_other"),
    };
    h_flex()
        .mt_2()
        .px_2()
        .pb_1()
        .gap_3()
        .border_b_1()
        .border_color(divider)
        .child(
            div()
                .font_weight(FontWeight::SEMIBOLD)
                .child(entry.size_label()),
        )
        .child(
            div()
                .flex_1()
                .text_xs()
                .text_color(muted)
                .child(description.into_owned()),
        )
        .when_some(entry.reference_quality(), |this, r| {
            this.child(
                div().text_xs().text_color(muted).child(
                    t!(
                        "models.bf16_reference",
                        ja = format!("{:.3}", r.pftb),
                        multi = format!("{:.1}", r.flores_ja)
                    )
                    .into_owned(),
                ),
            )
        })
        .into_any_element()
}

fn quant_note(quant: &str) -> Option<String> {
    let key = match quant {
        "Q4_K_M" => "models.quant_q4km",
        "Q4_0" => "models.quant_q40",
        "Q3_K_M" => "models.quant_q3km",
        "Q2_K" => "models.quant_q2k",
        "Q8_0" => "models.quant_q80",
        _ => return None,
    };
    Some(t!(key).into_owned())
}

/// Published quality of a file, with the difference from the unquantized model.
/// Larger drops are highlighted: thresholds roughly separate "same as BF16" from noticeable loss.
fn quality_cell(entry: &CatalogEntry, muted: Hsla, warning: Hsla, danger: Hsla) -> AnyElement {
    let (Some(q), Some(r)) = (entry.quality(), entry.reference_quality()) else {
        return div()
            .w(scaled(170.))
            .flex_shrink_0()
            .text_xs()
            .text_color(muted)
            .child(t!("models.quality_unknown").into_owned())
            .into_any_element();
    };
    let line = |label: String, value: String, delta: f32, warn: f32, bad: f32, digits: usize| {
        let color = if delta <= -bad {
            danger
        } else if delta <= -warn {
            warning
        } else {
            muted
        };
        h_flex()
            .gap_1()
            .child(div().text_color(muted).child(label))
            .child(div().child(value))
            .child(
                div()
                    .text_color(color)
                    .child(format!("({delta:+.digits$})")),
            )
    };
    v_flex()
        .w(scaled(170.))
        .flex_shrink_0()
        .text_xs()
        .child(line(
            t!("models.metric_ja").into_owned(),
            format!("{:.3}", q.pftb),
            q.pftb - r.pftb,
            0.003,
            0.008,
            3,
        ))
        .child(line(
            t!("models.metric_multi").into_owned(),
            format!("{:.1}", q.flores_ja),
            q.flores_ja - r.flores_ja,
            0.3,
            0.8,
            1,
        ))
        .into_any_element()
}

fn progress_text(p: &Progress) -> String {
    let mut text = format!(
        "{:.0}% · {} / {}",
        p.fraction() * 100.0,
        format_bytes(p.downloaded),
        format_bytes(p.total)
    );
    if p.bytes_per_second > 0.0 {
        text.push_str(&format!(" · {}/s", format_bytes(p.bytes_per_second as u64)));
    }
    if let Some(eta) = p.eta() {
        text.push_str(&format!(" · {}", crate::lang::format_duration(eta)));
    }
    text
}

pub fn format_bytes(bytes: u64) -> String {
    const MB: f64 = 1e6;
    const GB: f64 = 1e9;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.2} GB", b / GB)
    } else {
        format!("{:.0} MB", b / MB)
    }
}

#[cfg(test)]
mod tests {
    use super::format_bytes;

    #[test]
    fn formatting() {
        assert_eq!(format_bytes(1_807_192_480), "1.81 GB");
        assert_eq!(format_bytes(573_116_288), "573 MB");
    }
}
