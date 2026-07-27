//! Minimal `.desktop` entry parser/serializer.
//!
//! The Desktop Entry spec is a subset of INI: a `[Desktop Entry]` group,
//! `Key=Value` lines, comments start with `#`. We keep the implementation
//! intentionally small: only what this tool needs (read the upstream entry,
//! tweak a few keys, write a new one).
//!
//! Locale keys (`Key[lang]=...`) are preserved verbatim.
//!
//! Groups other than `[Desktop Entry]` — in practice `[Desktop Action <id>]`,
//! the "New Window"/"New Private Window" style entries KDE shows in the
//! launcher context menu — are preserved verbatim too. Dropping them while
//! keeping the `Actions=` key that references them would produce an entry that
//! violates the spec (§ Additional applications actions: every id listed in
//! `Actions` must have a matching group).

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

/// A group other than `[Desktop Entry]`, kept verbatim.
#[derive(Debug, Clone)]
pub struct Group {
    /// Header text without the brackets, e.g. `Desktop Action new-window`.
    pub name: String,
    pub keys: Vec<(String, String)>,
}

/// A parsed `.desktop` file: ordered key/value entries within the
/// `[Desktop Entry]` group, plus any further groups.
#[derive(Debug, Clone, Default)]
pub struct DesktopEntry {
    /// Preserves insertion order for stable, diff-friendly output.
    pub keys: Vec<(String, String)>,
    /// Additional groups, in file order. Not touched by [`Self::get`] /
    /// [`Self::set`], which operate on the main group only.
    pub groups: Vec<Group>,
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
                if !in_main_group {
                    entry.groups.push(Group {
                        name: line[1..line.len() - 1].to_string(),
                        keys: Vec::new(),
                    });
                }
                continue;
            }
            if let Some((k, v)) = split_kv(line) {
                if in_main_group {
                    entry.keys.push((k, v));
                } else if let Some(group) = entry.groups.last_mut() {
                    group.keys.push((k, v));
                }
                // A key before any group header is malformed; drop it.
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
        for group in &self.groups {
            writeln!(f, "\n[{}]", group.name)?;
            for (k, v) in &group.keys {
                writeln!(f, "{k}={v}")?;
            }
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

    #[test]
    fn preserves_action_groups() {
        let input = "\
[Desktop Entry]
Name=Foo
Actions=new-window;

[Desktop Action new-window]
Name=New Window
Exec=AppRun --new-window
";
        let e = DesktopEntry::parse(input);
        assert_eq!(e.groups.len(), 1);
        assert_eq!(e.groups[0].name, "Desktop Action new-window");
        let out = e.to_string();
        assert!(out.contains("[Desktop Action new-window]"));
        assert!(out.contains("Exec=AppRun --new-window"));
        // Re-parsing the output must yield the same shape.
        assert_eq!(DesktopEntry::parse(&out).groups.len(), 1);
    }
}
