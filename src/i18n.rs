//! Runtime locale selection.
//!
//! `rust_i18n::i18n!` bakes the catalogs into the binary but never looks at
//! the environment: without an explicit `set_locale` every message comes back
//! in the fallback language, whatever the user's session is set to. This
//! module reproduces the part of the POSIX lookup that matters for a desktop
//! tool — the first non-empty of `LC_ALL`, `LC_MESSAGES`, `LANG` — and
//! narrows the result to a locale we actually ship.

use std::env;

/// Environment variables consulted, in POSIX precedence order.
const LOCALE_VARS: [&str; 3] = ["LC_ALL", "LC_MESSAGES", "LANG"];

/// Pick the UI language from the environment and hand it to rust-i18n.
///
/// Called once from `main`. When nothing usable is found the global locale is
/// left alone, so `t!()` serves the `i18n!` fallback.
pub fn init() {
    let available = rust_i18n::available_locales!();
    if let Some(locale) = detect(|key| env::var(key).ok(), &available) {
        rust_i18n::set_locale(locale);
    }
}

/// Resolve the environment to one of `available`, or `None` to keep the
/// fallback.
///
/// `lookup` is injected so the tests can exercise this without touching the
/// process environment, which is global state shared with every other test.
fn detect<F, S>(lookup: F, available: &[S]) -> Option<&str>
where
    F: Fn(&str) -> Option<String>,
    S: AsRef<str>,
{
    // POSIX: the first variable that is set and non-empty decides. If it names
    // a language we do not ship we stop there rather than falling through to
    // the next variable — `LC_ALL=fr_FR LANG=it_IT` means French, and serving
    // Italian would ignore an explicit override.
    let raw = LOCALE_VARS
        .iter()
        .find_map(|var| lookup(var).filter(|value| !value.is_empty()))?;
    match_locale(&raw, available)
}

/// Match a POSIX locale string such as `it_IT.UTF-8@euro` against the shipped
/// catalogs.
fn match_locale<'a, S: AsRef<str>>(raw: &str, available: &'a [S]) -> Option<&'a str> {
    // Drop the codeset and the modifier: `it_IT.UTF-8@euro` -> `it_IT`.
    let tag = raw.split(['.', '@']).next().unwrap_or(raw);
    // The portable locales carry no language at all.
    if tag.is_empty() || tag == "C" || tag == "POSIX" {
        return None;
    }
    let tag = tag.replace('_', "-");
    let language = tag.split('-').next().unwrap_or(tag.as_str());

    // Region-specific catalog first (`pt-BR` beats `pt`), then the bare
    // language, so `pt_BR` still finds `pt` when only that one is shipped.
    [tag.as_str(), language].into_iter().find_map(|candidate| {
        available
            .iter()
            .map(AsRef::as_ref)
            .find(|locale| locale.eq_ignore_ascii_case(candidate))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build the `lookup` closure from a list of `(var, value)` pairs.
    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
        let pairs: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |key| pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
    }

    /// The locales this crate ships, as `available_locales!` would report them.
    const SHIPPED: [&str; 3] = ["en", "it", "es"];

    #[test]
    fn strips_codeset_and_modifier() {
        let got = detect(env(&[("LANG", "it_IT.UTF-8@euro")]), &SHIPPED);
        assert_eq!(got, Some("it"));
    }

    #[test]
    fn falls_back_from_region_to_language() {
        // `es_AR` is not shipped, but `es` is.
        let got = detect(env(&[("LANG", "es_AR.UTF-8")]), &SHIPPED);
        assert_eq!(got, Some("es"));
    }

    #[test]
    fn prefers_the_region_specific_catalog() {
        const AVAILABLE: [&str; 2] = ["pt", "pt-BR"];
        assert_eq!(detect(env(&[("LANG", "pt_BR")]), &AVAILABLE), Some("pt-BR"));
        assert_eq!(detect(env(&[("LANG", "pt_PT")]), &AVAILABLE), Some("pt"));
    }

    #[test]
    fn lc_all_outranks_lang() {
        let got = detect(
            env(&[("LC_ALL", "es_ES.UTF-8"), ("LANG", "it_IT.UTF-8")]),
            &SHIPPED,
        );
        assert_eq!(got, Some("es"));
    }

    #[test]
    fn an_empty_variable_does_not_count_as_set() {
        let got = detect(env(&[("LC_ALL", ""), ("LANG", "it_IT.UTF-8")]), &SHIPPED);
        assert_eq!(got, Some("it"));
    }

    #[test]
    fn an_explicit_override_is_not_second_guessed() {
        // LC_ALL wins even though we ship no French: falling through to LANG
        // would silently ignore the override.
        let got = detect(
            env(&[("LC_ALL", "fr_FR.UTF-8"), ("LANG", "it_IT.UTF-8")]),
            &SHIPPED,
        );
        assert_eq!(got, None);
    }

    #[test]
    fn the_portable_locales_use_the_fallback() {
        assert_eq!(detect(env(&[("LANG", "C")]), &SHIPPED), None);
        assert_eq!(detect(env(&[("LC_ALL", "POSIX")]), &SHIPPED), None);
    }

    #[test]
    fn an_empty_environment_uses_the_fallback() {
        assert_eq!(detect(env(&[]), &SHIPPED), None);
    }

    /// Regression guard for the catalog layout. A `_version`/nesting mismatch
    /// makes rust-i18n silently return the key itself instead of a message,
    /// which compiles, passes clippy, and breaks every string in the UI.
    #[test]
    fn every_shipped_catalog_actually_translates() {
        let available = rust_i18n::available_locales!();
        for expected in ["en", "it", "es"] {
            assert!(
                available.iter().any(|l| l == expected),
                "locale {expected} missing from {available:?}"
            );
        }

        for locale in ["en", "it", "es"] {
            let msg = rust_i18n::t!("msg_no_installs", locale = locale);
            assert_ne!(
                msg, "msg_no_installs",
                "{locale} catalog resolves to the bare key"
            );
            let msg = rust_i18n::t!("msg_removed", locale = locale, name = "Foo");
            assert!(
                msg.contains("Foo"),
                "{locale} catalog dropped the interpolation: {msg}"
            );
        }
    }

    /// Every locale we ship, source language first.
    const CATALOGS: [&str; 3] = ["it", "en", "es"];

    /// Read `locales/<locale>.yml` as `(key, raw value)` pairs.
    ///
    /// Deliberately not a YAML parse: the point is to inspect the files as
    /// written, including the placeholders inside block scalars, rather than
    /// whatever rust-i18n made of them.
    fn catalog(locale: &str) -> Vec<(String, String)> {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("locales/{locale}.yml"));
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{} should be readable: {e}", path.display()));

        let mut entries: Vec<(String, String)> = Vec::new();
        for line in text.lines() {
            let key = line.split_once(':').map(|(key, _)| key).filter(|key| {
                // Top level, so no indentation; `_version` is metadata.
                !key.is_empty()
                    && !key.starts_with(['_', '#', ' ', '\t'])
                    && key.chars().all(|c| c.is_ascii_lowercase() || c == '_')
            });
            match key {
                Some(key) => {
                    let value = line.split_once(':').map(|(_, value)| value).unwrap_or("");
                    entries.push((key.to_string(), value.to_string()));
                }
                // Continuation of the previous entry (block scalar, blank line).
                None => {
                    if let Some((_, value)) = entries.last_mut() {
                        value.push('\n');
                        value.push_str(line);
                    }
                }
            }
        }
        entries
    }

    /// The `%{…}` names a message interpolates, sorted and deduplicated.
    fn placeholders(value: &str) -> Vec<&str> {
        let mut found: Vec<&str> = value
            .match_indices("%{")
            .filter_map(|(at, _)| {
                let rest = &value[at + 2..];
                rest.find('}').map(|end| &rest[..end])
            })
            .collect();
        found.sort_unstable();
        found.dedup();
        found
    }

    /// A key present in the source language but missing elsewhere degrades to
    /// the `en` fallback silently — no warning at build time or at runtime.
    #[test]
    fn the_catalogs_cover_the_same_keys() {
        let source: Vec<String> = catalog(CATALOGS[0])
            .into_iter()
            .map(|(key, _)| key)
            .collect();
        assert!(!source.is_empty(), "the source catalog parsed as empty");

        for locale in &CATALOGS[1..] {
            let keys: Vec<String> = catalog(locale).into_iter().map(|(key, _)| key).collect();
            for key in &source {
                assert!(
                    keys.contains(key),
                    "locales/{locale}.yml is missing `{key}`"
                );
            }
            for key in &keys {
                assert!(
                    source.contains(key),
                    "locales/{locale}.yml has `{key}`, absent from locales/{}.yml",
                    CATALOGS[0]
                );
            }
        }
    }

    /// rust-i18n interpolates `%{name}` — `{{name}}` and friends are copied to
    /// the output verbatim, and a dropped placeholder swallows the path or the
    /// error the message was supposed to carry.
    #[test]
    fn every_message_keeps_its_placeholders() {
        let source = catalog(CATALOGS[0]);

        for locale in CATALOGS {
            for (key, value) in catalog(locale) {
                assert!(
                    !value.contains("{{"),
                    "locales/{locale}.yml `{key}` uses `{{{{…}}}}`; rust-i18n only \
                     interpolates `%{{…}}`"
                );
                let expected = source
                    .iter()
                    .find(|(source_key, _)| *source_key == key)
                    .map(|(_, source_value)| placeholders(source_value))
                    .unwrap_or_default();
                assert_eq!(
                    placeholders(&value),
                    expected,
                    "locales/{locale}.yml `{key}` does not interpolate the same names \
                     as locales/{}.yml",
                    CATALOGS[0]
                );
            }
        }
    }
}
