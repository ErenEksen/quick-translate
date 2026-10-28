//! Supported languages (DeepL codes) and locale-based default target.

/// A language with its DeepL code and English display name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Language {
    /// DeepL code, e.g. `TR`, `EN-US`, `PT-BR`, `DE`.
    pub code: &'static str,
    pub name: &'static str,
}

const fn lang(code: &'static str, name: &'static str) -> Language {
    Language { code, name }
}

/// The fallback target when the locale is unset or unsupported.
pub const DEFAULT_TARGET: &str = "EN-US";

static TARGETS: &[Language] = &[
    lang("AR", "Arabic"),
    lang("BG", "Bulgarian"),
    lang("CS", "Czech"),
    lang("DA", "Danish"),
    lang("DE", "German"),
    lang("EL", "Greek"),
    lang("EN-GB", "English (British)"),
    lang("EN-US", "English (American)"),
    lang("ES", "Spanish"),
    lang("ES-419", "Spanish (Latin American)"),
    lang("ET", "Estonian"),
    lang("FI", "Finnish"),
    lang("FR", "French"),
    lang("HE", "Hebrew"),
    lang("HU", "Hungarian"),
    lang("ID", "Indonesian"),
    lang("IT", "Italian"),
    lang("JA", "Japanese"),
    lang("KO", "Korean"),
    lang("LT", "Lithuanian"),
    lang("LV", "Latvian"),
    lang("NB", "Norwegian Bokmål"),
    lang("NL", "Dutch"),
    lang("PL", "Polish"),
    lang("PT-BR", "Portuguese (Brazilian)"),
    lang("PT-PT", "Portuguese (European)"),
    lang("RO", "Romanian"),
    lang("RU", "Russian"),
    lang("SK", "Slovak"),
    lang("SL", "Slovenian"),
    lang("SV", "Swedish"),
    lang("TH", "Thai"),
    lang("TR", "Turkish"),
    lang("UK", "Ukrainian"),
    lang("VI", "Vietnamese"),
    lang("ZH-HANS", "Chinese (simplified)"),
    lang("ZH-HANT", "Chinese (traditional)"),
];

static SOURCES: &[Language] = &[
    lang("AR", "Arabic"),
    lang("BG", "Bulgarian"),
    lang("CS", "Czech"),
    lang("DA", "Danish"),
    lang("DE", "German"),
    lang("EL", "Greek"),
    lang("EN", "English"),
    lang("ES", "Spanish"),
    lang("ET", "Estonian"),
    lang("FI", "Finnish"),
    lang("FR", "French"),
    lang("HE", "Hebrew"),
    lang("HU", "Hungarian"),
    lang("ID", "Indonesian"),
    lang("IT", "Italian"),
    lang("JA", "Japanese"),
    lang("KO", "Korean"),
    lang("LT", "Lithuanian"),
    lang("LV", "Latvian"),
    lang("NB", "Norwegian Bokmål"),
    lang("NL", "Dutch"),
    lang("PL", "Polish"),
    lang("PT", "Portuguese"),
    lang("RO", "Romanian"),
    lang("RU", "Russian"),
    lang("SK", "Slovak"),
    lang("SL", "Slovenian"),
    lang("SV", "Swedish"),
    lang("TH", "Thai"),
    lang("TR", "Turkish"),
    lang("UK", "Ukrainian"),
    lang("VI", "Vietnamese"),
    lang("ZH", "Chinese"),
];

/// Target languages accepted by DeepL `/v2/translate` `target_lang`.
pub fn target_languages() -> &'static [Language] {
    TARGETS
}

/// Source languages accepted by DeepL `source_lang` (no regional variants).
pub fn source_languages() -> &'static [Language] {
    SOURCES
}

fn find(list: &'static [Language], code: &str) -> Option<Language> {
    list.iter()
        .find(|l| l.code.eq_ignore_ascii_case(code.trim()))
        .copied()
}

/// Look up a target language by code (case-insensitive).
pub fn target_by_code(code: &str) -> Option<Language> {
    find(TARGETS, code)
}

/// Look up a source language by code (case-insensitive).
pub fn source_by_code(code: &str) -> Option<Language> {
    find(SOURCES, code)
}

/// Look up a target or source language by code (case-insensitive).
pub fn by_code(code: &str) -> Option<Language> {
    target_by_code(code).or_else(|| source_by_code(code))
}

/// Map a source code to the matching target code for swap (`EN` -> `EN-US`, `PT` -> `PT-BR`),
/// and a target code to its source code (`EN-GB` -> `EN`).
pub fn source_to_target(code: &str) -> Option<&'static str> {
    let code = code.trim();
    let mapped = if code.eq_ignore_ascii_case("EN") {
        "EN-US"
    } else if code.eq_ignore_ascii_case("PT") {
        "PT-BR"
    } else if code.eq_ignore_ascii_case("ZH") {
        "ZH-HANS"
    } else {
        return source_by_code(code).and_then(|l| target_by_code(l.code).map(|t| t.code));
    };
    Some(mapped)
}

pub fn target_to_source(code: &str) -> Option<&'static str> {
    let target = target_by_code(code)?;
    let base = target.code.split('-').next().unwrap_or(target.code);
    source_by_code(base).map(|l| l.code)
}

/// Default target from `LC_ALL`, then `LC_MESSAGES`, then `LANG`; English (`EN-US`) if unset,
/// `C`/`POSIX`, or unsupported.
pub fn default_target_from_env() -> &'static str {
    default_target_from(|key| std::env::var(key).ok())
}

/// Testable variant of [`default_target_from_env`] with an injected environment lookup.
pub fn default_target_from(env: impl Fn(&str) -> Option<String>) -> &'static str {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .into_iter()
        .filter_map(&env)
        .find(|v| !v.trim().is_empty())
        .and_then(|locale| target_for_locale(&locale))
        .unwrap_or(DEFAULT_TARGET)
}

/// Map a POSIX locale name such as `tr_TR.UTF-8` or `pt_BR` to a DeepL target code.
pub fn target_for_locale(locale: &str) -> Option<&'static str> {
    let locale = locale.trim();
    let without_modifier = locale.split('@').next().unwrap_or(locale);
    let name = without_modifier
        .split('.')
        .next()
        .unwrap_or(without_modifier);
    let mut parts = name.split(['_', '-']);
    let language = parts.next()?.to_ascii_lowercase();
    let territory = parts.next().unwrap_or("").to_ascii_uppercase();

    let code = match (language.as_str(), territory.as_str()) {
        ("" | "c" | "posix", _) => return None,
        ("en", "GB") => "EN-GB",
        ("en", _) => "EN-US",
        ("pt", "BR") => "PT-BR",
        ("pt", _) => "PT-PT",
        ("zh", "TW" | "HK" | "MO") => "ZH-HANT",
        ("zh", _) => "ZH-HANS",
        ("nb" | "nn" | "no", _) => "NB",
        ("he" | "iw", _) => "HE",
        (other, _) => return target_by_code(other).map(|l| l.code),
    };
    Some(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(vars: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |key| {
            vars.iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| (*v).to_owned())
        }
    }

    #[test]
    fn turkish_locale() {
        assert_eq!(default_target_from(env(&[("LANG", "tr_TR.UTF-8")])), "TR");
    }

    #[test]
    fn regional_variants() {
        assert_eq!(default_target_from(env(&[("LANG", "pt_BR")])), "PT-BR");
        assert_eq!(
            default_target_from(env(&[("LANG", "pt_PT.UTF-8")])),
            "PT-PT"
        );
        assert_eq!(
            default_target_from(env(&[("LANG", "en_GB.UTF-8")])),
            "EN-GB"
        );
        assert_eq!(
            default_target_from(env(&[("LANG", "en_AU.UTF-8")])),
            "EN-US"
        );
        assert_eq!(default_target_from(env(&[("LANG", "en")])), "EN-US");
        assert_eq!(
            default_target_from(env(&[("LANG", "zh_TW.UTF-8")])),
            "ZH-HANT"
        );
        assert_eq!(default_target_from(env(&[("LANG", "zh_HK")])), "ZH-HANT");
        assert_eq!(
            default_target_from(env(&[("LANG", "zh_CN.UTF-8")])),
            "ZH-HANS"
        );
        assert_eq!(default_target_from(env(&[("LANG", "de_DE@euro")])), "DE");
        assert_eq!(default_target_from(env(&[("LANG", "nb_NO.UTF-8")])), "NB");
    }

    #[test]
    fn c_and_posix_fall_back_to_english() {
        assert_eq!(default_target_from(env(&[("LANG", "C")])), "EN-US");
        assert_eq!(default_target_from(env(&[("LANG", "C.UTF-8")])), "EN-US");
        assert_eq!(default_target_from(env(&[("LANG", "POSIX")])), "EN-US");
    }

    #[test]
    fn unset_empty_and_unsupported_fall_back_to_english() {
        assert_eq!(default_target_from(env(&[])), "EN-US");
        assert_eq!(
            default_target_from(env(&[("LC_ALL", ""), ("LANG", "")])),
            "EN-US"
        );
        assert_eq!(
            default_target_from(env(&[("LANG", "xx_YY.UTF-8")])),
            "EN-US"
        );
    }

    #[test]
    fn posix_precedence() {
        let vars = [
            ("LC_ALL", "de_DE.UTF-8"),
            ("LC_MESSAGES", "fr_FR.UTF-8"),
            ("LANG", "tr_TR.UTF-8"),
        ];
        assert_eq!(default_target_from(env(&vars)), "DE");
        let vars = [
            ("LC_ALL", ""),
            ("LC_MESSAGES", "fr_FR.UTF-8"),
            ("LANG", "tr_TR.UTF-8"),
        ];
        assert_eq!(default_target_from(env(&vars)), "FR");
        let vars = [("LC_MESSAGES", "  "), ("LANG", "tr_TR.UTF-8")];
        assert_eq!(default_target_from(env(&vars)), "TR");
        // The first non-empty value wins even when it is `C`.
        let vars = [("LC_ALL", "C"), ("LANG", "tr_TR.UTF-8")];
        assert_eq!(default_target_from(env(&vars)), "EN-US");
    }

    #[test]
    fn lists_are_consistent() {
        for list in [target_languages(), source_languages()] {
            let mut codes: Vec<_> = list.iter().map(|l| l.code).collect();
            let len = codes.len();
            codes.sort_unstable();
            codes.dedup();
            assert_eq!(codes.len(), len, "duplicate codes");
        }
        for code in [
            "EN-US", "EN-GB", "PT-BR", "PT-PT", "ZH-HANS", "ZH-HANT", "TR", "DE",
        ] {
            assert!(target_by_code(code).is_some(), "{code}");
        }
        for t in target_languages() {
            assert!(target_to_source(t.code).is_some(), "{}", t.code);
        }
        for s in source_languages() {
            assert!(source_to_target(s.code).is_some(), "{}", s.code);
        }
        assert!(source_languages().iter().all(|l| !l.code.contains('-')));
    }

    #[test]
    fn lookup_and_swap_mapping() {
        assert_eq!(by_code("tr").map(|l| l.name), Some("Turkish"));
        assert_eq!(by_code("zh-hant").map(|l| l.code), Some("ZH-HANT"));
        assert_eq!(by_code("EN").map(|l| l.code), Some("EN"));
        assert_eq!(by_code("XX"), None);
        assert_eq!(source_to_target("EN"), Some("EN-US"));
        assert_eq!(source_to_target("pt"), Some("PT-BR"));
        assert_eq!(source_to_target("ZH"), Some("ZH-HANS"));
        assert_eq!(source_to_target("DE"), Some("DE"));
        assert_eq!(source_to_target("XX"), None);
        assert_eq!(target_to_source("EN-GB"), Some("EN"));
        assert_eq!(target_to_source("PT-PT"), Some("PT"));
        assert_eq!(target_to_source("ZH-HANT"), Some("ZH"));
        assert_eq!(target_to_source("ES-419"), Some("ES"));
        assert_eq!(target_to_source("TR"), Some("TR"));
        assert_eq!(target_to_source("EN"), None);
    }
}
