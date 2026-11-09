//! Choices offered in selects (languages, style, UI language, theme, token counts) and
//! localized formatting helpers.

use std::time::Duration;

use engine::pfml::{Form, JA_EASY, LANGUAGES, find_language};
use gpui_kit::SharedString;
use gpui_kit::component::select::SelectItem;
use rust_i18n::t;

use crate::settings::{ThemeSetting, UiLanguage};

/// Source language choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceChoice {
    /// Let the model detect the language.
    Detect,
    Lang(&'static str),
}

impl SourceChoice {
    pub fn code(self) -> Option<&'static str> {
        match self {
            Self::Detect => None,
            Self::Lang(code) => Some(code),
        }
    }
}

/// Target language choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetChoice {
    /// Japanese text goes to English and everything else to Japanese.
    JapaneseEnglish,
    Lang(&'static str),
}

impl TargetChoice {
    /// The concrete target language for a source text, given whether it [`looks_japanese`].
    pub fn resolve_for(self, source_is_japanese: bool) -> &'static str {
        match self {
            Self::Lang(code) => code,
            Self::JapaneseEnglish if source_is_japanese => "en",
            Self::JapaneseEnglish => "ja",
        }
    }
}

/// Japanese output style.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StyleChoice {
    /// Leave the style to the model.
    #[default]
    Default,
    Polite,
    Plain,
}

impl StyleChoice {
    pub fn form(self) -> Option<Form> {
        match self {
            Self::Default => None,
            Self::Polite => Some(Form::Polite),
            Self::Plain => Some(Form::Plain),
        }
    }
}

pub use appdata::looks_japanese;

/// Localized display name of a language code.
pub fn language_name(code: &str) -> String {
    let key = format!("lang.{}", code.replace('-', "_").replace("(easy)", "_easy"));
    let name = t!(&key);
    if name == key {
        // Missing translation: fall back to the English name from the model card.
        find_language(code).map_or_else(|| code.to_owned(), |l| l.english_name.to_owned())
    } else {
        name.into_owned()
    }
}

/// An entry in a select.
#[derive(Debug, Clone)]
pub struct Choice<V> {
    value: V,
    title: SharedString,
    /// Lowercased text matched against the search query.
    keywords: String,
}

impl<V> Choice<V> {
    pub fn new(value: V, title: String, extra_keywords: &[&str]) -> Self {
        let mut keywords = title.to_lowercase();
        for k in extra_keywords {
            keywords.push('\n');
            keywords.push_str(&k.to_lowercase());
        }
        Self {
            value,
            title: title.into(),
            keywords,
        }
    }

    fn language(value: V, code: &'static str) -> Self {
        let mut extra = vec![code];
        if let Some(l) = find_language(code) {
            extra.extend([l.english_name, l.native_name]);
        }
        Self::new(value, language_name(code), &extra)
    }
}

impl<V: Clone + PartialEq> SelectItem for Choice<V> {
    type Value = V;

    fn title(&self) -> SharedString {
        self.title.clone()
    }

    fn value(&self) -> &V {
        &self.value
    }

    fn matches(&self, query: &str) -> bool {
        self.keywords.contains(&query.to_lowercase())
    }
}

pub fn source_choices() -> Vec<Choice<SourceChoice>> {
    let mut choices = vec![Choice::new(
        SourceChoice::Detect,
        t!("source.detect").into_owned(),
        &["auto"],
    )];
    choices.extend(
        LANGUAGES
            .iter()
            .map(|l| Choice::language(SourceChoice::Lang(l.code), l.code)),
    );
    choices
}

pub fn target_choices() -> Vec<Choice<TargetChoice>> {
    let mut choices = vec![Choice::new(
        TargetChoice::JapaneseEnglish,
        t!("target.ja_en").into_owned(),
        &["auto", "ja", "en"],
    )];
    choices.extend(
        LANGUAGES
            .iter()
            .map(|l| Choice::language(TargetChoice::Lang(l.code), l.code)),
    );
    choices.push(Choice::language(TargetChoice::Lang(JA_EASY), JA_EASY));
    choices
}

/// UI language options. Languages are named in their own language.
pub fn ui_language_choices() -> Vec<Choice<UiLanguage>> {
    let system = match crate::settings::system_locale() {
        "ja" => "日本語",
        _ => "English",
    };
    vec![
        Choice::new(
            UiLanguage::System,
            t!("settings.language_system", lang = system).into_owned(),
            &[],
        ),
        Choice::new(UiLanguage::English, "English".to_owned(), &[]),
        Choice::new(UiLanguage::Japanese, "日本語".to_owned(), &[]),
    ]
}

/// Chunk size options, in tokens. Chunks are also capped at a sixth of the context, so larger
/// values would not take effect.
pub const CHUNK_TOKEN_PRESETS: &[usize] = &[256, 512, 1024, 2048, 4096];

/// History size options, in tokens. History is also capped at a third of the context.
pub const HISTORY_TOKEN_PRESETS: &[usize] = &[0, 1024, 2048, 4096, 8192];

/// The interface sizes offered, in percent.
pub const UI_SCALE_PRESETS: &[u16] = &[100, 125, 150, 175, 200];

/// Interface size options: the presets, plus `current` if it is not one of them (a value edited
/// in settings.json). 100% is marked as the default.
pub fn ui_scale_choices(current: u16) -> Vec<Choice<u16>> {
    let mut values = UI_SCALE_PRESETS.to_vec();
    if !values.contains(&current) {
        values.push(current);
        values.sort_unstable();
    }
    values
        .into_iter()
        .map(|n| {
            let title = format!("{n}%");
            let title = if n == 100 {
                t!("settings.default", value = title).into_owned()
            } else {
                title
            };
            Choice::new(n, title, &[])
        })
        .collect()
}

/// Token count options: `presets`, plus `current` if it is not one of them (a value edited in
/// settings.json). `default` is marked.
pub fn token_choices(presets: &[usize], current: usize, default: usize) -> Vec<Choice<usize>> {
    let mut values = presets.to_vec();
    if !values.contains(&current) {
        values.push(current);
        values.sort_unstable();
    }
    values
        .into_iter()
        .map(|n| {
            let title = if n == 0 {
                t!("settings.history_none").into_owned()
            } else {
                t!("settings.tokens", count = n).into_owned()
            };
            let title = if n == default {
                t!("settings.default", value = title).into_owned()
            } else {
                title
            };
            Choice::new(n, title, &[])
        })
        .collect()
}

/// Appearance options.
pub fn theme_choices() -> Vec<Choice<ThemeSetting>> {
    [
        (ThemeSetting::System, "settings.theme_system"),
        (ThemeSetting::Light, "settings.theme_light"),
        (ThemeSetting::Dark, "settings.theme_dark"),
    ]
    .into_iter()
    .map(|(value, key)| Choice::new(value, t!(key).into_owned(), &[]))
    .collect()
}

pub fn style_choices() -> Vec<Choice<StyleChoice>> {
    [
        (StyleChoice::Default, "style.default"),
        (StyleChoice::Polite, "style.polite"),
        (StyleChoice::Plain, "style.plain"),
    ]
    .into_iter()
    .map(|(value, key)| Choice::new(value, t!(key).into_owned(), &[]))
    .collect()
}

/// Index of `value` in `choices`.
pub fn position<V: PartialEq>(choices: &[Choice<V>], value: &V) -> Option<usize> {
    choices.iter().position(|c| &c.value == value)
}

/// A duration such as `45 s`, `3 min 20 s`, or `1 h 05 min`.
pub fn format_duration(d: Duration) -> String {
    let secs = d.as_secs_f64().round() as u64;
    if secs < 60 {
        t!("time.seconds", s = secs).into_owned()
    } else if secs < 3600 {
        t!(
            "time.minutes",
            m = secs / 60,
            s = format!("{:02}", secs % 60)
        )
        .into_owned()
    } else {
        let minutes = secs / 60;
        t!(
            "time.hours",
            h = minutes / 60,
            m = format!("{:02}", minutes % 60)
        )
        .into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_japanese_by_kana() {
        assert!(looks_japanese("会議は来週です"));
        assert!(looks_japanese("カタカナ"));
        assert!(!looks_japanese("Hello, world"));
        // Kanji only: could be Chinese.
        assert!(!looks_japanese("你好世界"));
    }

    /// Serializes the tests that change the process-wide locale.
    static LOCALE: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn formats_durations() {
        let _locale = LOCALE.lock().unwrap_or_else(|e| e.into_inner());
        rust_i18n::set_locale("en");
        assert_eq!(format_duration(Duration::from_secs(45)), "45 s");
        assert_eq!(format_duration(Duration::from_secs(75)), "1 min 15 s");
        assert_eq!(format_duration(Duration::from_secs(3725)), "1 h 02 min");
    }

    #[test]
    fn japanese_english_pair() {
        assert_eq!(
            TargetChoice::JapaneseEnglish.resolve_for(looks_japanese("こんにちは")),
            "en"
        );
        assert_eq!(
            TargetChoice::JapaneseEnglish.resolve_for(looks_japanese("Hello")),
            "ja"
        );
        assert_eq!(
            TargetChoice::JapaneseEnglish.resolve_for(looks_japanese("你好")),
            "ja"
        );
        assert_eq!(
            TargetChoice::Lang("fr").resolve_for(looks_japanese("こんにちは")),
            "fr"
        );
    }

    #[test]
    fn every_language_has_a_localized_name() {
        let _locale = LOCALE.lock().unwrap_or_else(|e| e.into_inner());
        for locale in ["en", "ja"] {
            rust_i18n::set_locale(locale);
            for l in LANGUAGES {
                let key = format!("lang.{}", l.code.replace('-', "_"));
                assert_ne!(t!(&key), key, "missing {key} for {locale}");
            }
            assert_ne!(language_name(JA_EASY), JA_EASY);
        }
        rust_i18n::set_locale("en");
    }
}
