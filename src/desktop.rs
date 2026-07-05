//! Minimal `.desktop` entry parser/serializer.
//!
//! The Desktop Entry spec is a subset of INI: a single `[Desktop Entry]` group,
//! `Key=Value` lines, comments start with `#`. We keep the implementation
//! intentionally small: only what this tool needs (read the upstream entry,
//! tweak a few keys, write a new one).
//!
//! Locale keys (`Key[lang]=...`) are preserved verbatim.

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

/// A parsed `.desktop` file: ordered key/value entries within the
/// `[Desktop Entry]` group.
#[derive(Debug, Clone, Default)]
pub struct DesktopEntry {
    /// Preserves insertion order for stable, diff-friendly output.
    pub keys: Vec<(String, String)>,
}

impl DesktopEntry {
    /// Parse a `.desktop` file from UTF-8 bytes.
    pub fn parse(content: &str) -> Self {
        let mut entry = DesktopEntry::default();
        let mut in_main_group = false;

        for raw in content.lines() {
            let line = raw.trim_end();
            // Skip blank lines and comments, but keep them out of the model.
            if line.is_empty() || line.trim_start().starts_with('#') {
                continue;
            }
            if line.starts_with('[') && line.ends_with(']') {
                in_main_group = line == "[Desktop Entry]";
                continue;
            }
            if !in_main_group {
                continue;
            }
            if let Some((k, v)) = split_kv(line) {
                entry.keys.push((k, v));
            }
        }
        entry
    }

    /// Read from disk.
    #[allow(dead_code)]
    pub fn read(path: &Path) -> std::io::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        Ok(Self::parse(&content))
    }

    /// Get the first value for `key` (case-sensitive, as the spec requires).
    pub fn get(&self, key: &str) -> Option<&str> {
        self.keys
            .iter()
            .find_map(|(k, v)| if k == key { Some(v.as_str()) } else { None })
    }

    /// Set `key` to `value`. Updates the first existing occurrence, or appends.
    pub fn set(&mut self, key: &str, value: &str) {
        if let Some(slot) = self.keys.iter_mut().find(|(k, _)| k == key) {
            slot.1 = value.to_string();
        } else {
            self.keys.push((key.to_string(), value.to_string()));
        }
    }

    /// Remove all entries matching `key`.
    pub fn remove(&mut self, key: &str) {
        self.keys.retain(|(k, _)| k != key);
    }

    /// Returns a deduplicated view as a map (last value wins for dupes).
    #[allow(dead_code)] // useful for inspection/debugging
    pub fn as_map(&self) -> BTreeMap<&str, &str> {
        self.keys
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect()
    }
}

/// Split a `Key=Value` line, trimming the key and keeping the value as-is
/// (the spec says values are not trimmed on the right; trailing spaces matter
/// only for a few keys, none of which we set).
fn split_kv(line: &str) -> Option<(String, String)> {
    let eq = line.find('=')?;
    let key = line[..eq].trim().to_string();
    if key.is_empty() {
        return None;
    }
    let value = &line[eq + 1..];
    Some((key, value.to_string()))
}

impl fmt::Display for DesktopEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "[Desktop Entry]")?;
        for (k, v) in &self.keys {
            writeln!(f, "{k}={v}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_roundtrip() {
        let input = "\
[Desktop Entry]
Name=ZCode
Exec=AppRun --no-sandbox %U
Type=Application
Icon=zcode
# a comment
Categories=Development;
";
        let mut e = DesktopEntry::parse(input);
        assert_eq!(e.get("Name"), Some("ZCode"));
        assert_eq!(e.get("Exec"), Some("AppRun --no-sandbox %U"));
        e.set("Exec", "/home/u/.local/bin/zcode.AppImage --no-sandbox %U");
        e.set("X-AppImage-Manager", "true");
        let out = e.to_string();
        assert!(out.contains("Exec=/home/u/.local/bin/zcode.AppImage --no-sandbox %U"));
        assert!(out.contains("X-AppImage-Manager=true"));
        assert!(out.contains("[Desktop Entry]"));
    }

    #[test]
    fn ignores_non_main_group() {
        let input = "\
[Desktop Entry]
Name=Foo
Bar=1

[Desktop Action Open]
Exec=foo --open
";
        let e = DesktopEntry::parse(input);
        assert_eq!(e.get("Name"), Some("Foo"));
        assert_eq!(e.get("Bar"), Some("1"));
        assert!(e.get("Exec").is_none(), "must not pick up actions group");
    }
}
