//! UI string internationalization (zh / en), hand-rolled with no extra
//! dependencies.
//!
//! Every user-visible string lives in the `STRINGS` table below as
//! `(key, zh, en)`. Keys describe stable intent (`dialog.export.title`),
//! never a source-language sentence; each locale gets a full-sentence
//! template with sequential `{}` placeholders for dynamic values.
//!
//! Usage in views: `i18n::tr("sidebar.tab.schema")` for plain strings,
//! `i18n::trf("status_bar.last_query", &[&elapsed, &rows, &cols])` for
//! templates (extra `{}` placeholders are left untouched).
//!
//! Language switching (for the title_bar language-toggle UI):
//! call `i18n::set_language(Language::En)` — it updates the global and
//! persists the choice to the settings file (blocking write, fine from a
//! click handler) — then call `cx.refresh_windows()` so every view
//! re-renders in the new language without per-view subscriptions.

use std::collections::HashMap;
use std::sync::{LazyLock, RwLock};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Language {
    Zh,
    En,
}

impl Language {
    pub fn code(&self) -> &'static str {
        match self {
            Language::Zh => "zh",
            Language::En => "en",
        }
    }

    /// Unknown codes fall back to Chinese, the language the PRD was written in.
    pub fn from_code(code: &str) -> Language {
        match code {
            "en" => Language::En,
            _ => Language::Zh,
        }
    }
}

static CURRENT: RwLock<Language> = RwLock::new(Language::Zh);

/// What joins the parts of a list in running text: `, ` in English, the
/// full-width `，` in Chinese. The interface writes lists as text, never
/// with a middle dot.
pub fn sep() -> &'static str {
    match current() {
        Language::Zh => "，",
        Language::En => ", ",
    }
}

/// `a` and `b` as one line. In English a part that is its own sentence
/// ("Click to filter …") follows a full stop, not a comma.
pub fn join(a: &str, b: &str) -> String {
    join_in(current(), a, b)
}

fn join_in(language: Language, a: &str, b: &str) -> String {
    let a = a.trim_end_matches(['.', ',', '，', '。']);
    let between = match language {
        Language::En if b.starts_with(char::is_uppercase) => ". ",
        Language::En => ", ",
        Language::Zh => "，",
    };
    format!("{a}{between}{b}")
}

/// `parts` as one line, joined as [`join`] joins two; `None` when empty.
pub fn join_all<S: AsRef<str>>(parts: &[S]) -> Option<String> {
    let mut parts = parts.iter().map(AsRef::as_ref);
    let first = parts.next()?.to_string();
    Some(parts.fold(first, |line, part| join(&line, part)))
}

pub fn current() -> Language {
    *CURRENT.read().unwrap_or_else(|e| e.into_inner())
}

pub fn set_current(lang: Language) {
    *CURRENT.write().unwrap_or_else(|e| e.into_inner()) = lang;
}

/// Language to use at startup: the persisted `language` setting when the
/// settings file is reachable, otherwise the detected system locale. Never
/// persists anything — that only happens on an explicit user switch.
pub fn initial_language() -> Language {
    if let Ok(Some(code)) = crate::prefs::get("language") {
        return Language::from_code(&code);
    }
    detect_system_language()
}

/// Best-effort system locale detection: LC_ALL / LANG containing "zh"
/// (`zh_CN.UTF-8`, `zh-Hans`, …) means Chinese, anything else English.
pub fn detect_system_language() -> Language {
    let locale = std::env::var("LC_ALL")
        .ok()
        .filter(|value| !value.is_empty())
        .or_else(|| std::env::var("LANG").ok())
        .unwrap_or_default()
        .to_lowercase();
    if locale.contains("zh") {
        Language::Zh
    } else {
        Language::En
    }
}

/// Switch the UI language and persist the choice. Blocking (writes to the
/// settings file); callers on the UI thread should follow with
/// `cx.refresh_windows()` to repaint all views in the new language.
pub fn set_language(lang: Language) {
    set_current(lang);
    if let Err(e) = crate::prefs::set("language", lang.code()) {
        tracing::warn!("Failed to persist language setting: {e}");
    }
}

static ZH: LazyLock<HashMap<&'static str, &'static str>> =
    LazyLock::new(|| STRINGS.iter().map(|(key, zh, _)| (*key, *zh)).collect());
static EN: LazyLock<HashMap<&'static str, &'static str>> =
    LazyLock::new(|| STRINGS.iter().map(|(key, _, en)| (*key, *en)).collect());

/// Translate `key` in the current language, falling back to the zh value,
/// then to the key itself. Never panics on a missing key.
pub fn tr(key: &'static str) -> &'static str {
    translate(current(), key)
}

fn translate(lang: Language, key: &'static str) -> &'static str {
    let map = match lang {
        Language::Zh => &*ZH,
        Language::En => &*EN,
    };
    map.get(key).or_else(|| ZH.get(key)).copied().unwrap_or(key)
}

/// Translate `key` and substitute sequential `{}` placeholders with `args`
/// in order. Leftover `{}` placeholders stay as-is; extra args are ignored.
pub fn trf(key: &'static str, args: &[&str]) -> String {
    format_template(tr(key), args)
}

/// [`trf`] in a given language rather than the UI's: the CLI speaks English
/// whatever the window does.
pub fn trf_in(lang: Language, key: &'static str, args: &[&str]) -> String {
    format_template(translate(lang, key), args)
}

fn format_template(template: &str, args: &[&str]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    for arg in args {
        match rest.split_once("{}") {
            Some((before, after)) => {
                out.push_str(before);
                out.push_str(arg);
                rest = after;
            }
            None => break,
        }
    }
    out.push_str(rest);
    out
}

/// `(key, zh, en)` for every user-visible string, grouped by view.
static STRINGS: &[(&str, &str, &str)] = &[
    // ── Common ──────────────────────────────────────────────────────────
    ("common.cancel", "取消", "Cancel"),
    ("common.close", "关闭", "Close"),
    ("app.tagline", "HawDB 图数据的夜行者工作台", "A night traveler's workbench for HawDB graphs"),

    // ── sidebar ─────────────────────────────────────────────────────────
    ("sidebar.tab.schema", "结构", "Schema"),
    ("sidebar.tab.history", "查询历史", "History"),
];

#[cfg(test)]
mod tests {
    use super::{format_template, join_in, translate, Language};

    #[test]
    fn a_sentence_follows_a_full_stop_not_a_comma() {
        assert_eq!(
            join_in(Language::En, "trips by payment", "Click to filter"),
            "trips by payment. Click to filter"
        );
        assert_eq!(join_in(Language::Zh, "按标签", "点击可筛选"), "按标签，点击可筛选");
    }

    #[test]
    fn translate_falls_back_to_zh_then_key() {
        assert_eq!(translate(Language::En, "sidebar.tab.schema"), "Schema");
        assert_eq!(translate(Language::Zh, "sidebar.tab.schema"), "结构");
        assert_eq!(translate(Language::En, "no.such.key"), "no.such.key");
    }

    #[test]
    fn every_key_has_both_locales_and_no_duplicates() {
        let mut seen = std::collections::HashSet::new();
        for (key, zh, en) in super::STRINGS {
            assert!(seen.insert(key), "duplicate key: {key}");
            assert!(!zh.is_empty(), "empty zh for {key}");
            assert!(!en.is_empty(), "empty en for {key}");
        }
    }

    #[test]
    fn format_template_substitutes_sequentially() {
        assert_eq!(format_template("{} rows · {} cols", &["3", "2"]), "3 rows · 2 cols");
        assert_eq!(format_template("{} {}", &["a"]), "a {}");
    }
}
