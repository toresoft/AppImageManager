//! Persistent record of the AppImages we installed.
//!
//! Until 0.1.3 the `.desktop` file *was* the database: `list` and `uninstall`
//! found our installs by scanning `~/.local/share/applications` for entries
//! carrying the `X-AppImage-Manager` marker. That coupling is what forced the
//! `.desktop` filename to be namespaced, which in turn produced duplicate menu
//! entries (see `installer::prune_duplicate_entries`).
//!
//! Keeping our bookkeeping in a file of our own decouples the two concerns:
//! the `.desktop` can use the canonical, spec-conventional file ID, and an
//! application that rewrites its own entry cannot make an install invisible.
//!
//! Format is a small INI-like text file — no dependency, and readable/fixable
//! by hand:
//!
//! ```text
//! [zcode]
//! display=ZCode
//! binary=/home/u/.local/bin/zcode.AppImage
//! desktop=zcode.desktop
//! icon=zcode
//! source=/home/u/Downloads/ZCode-3.5.3-x86_64.AppImage
//! ```

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// One installed AppImage.
#[derive(Debug, Clone, Default)]
pub struct Entry {
    /// Canonical install name (e.g. `zcode`), also the group header.
    pub name: String,
    /// Human-readable name from the desktop entry.
    pub display_name: String,
    /// Absolute path of the installed executable under `~/.local/bin`.
    pub binary: PathBuf,
    /// File name (not path) of the menu entry under `~/.local/share/applications`.
    pub desktop: String,
    /// Icon theme name we installed the icons under.
    pub icon: String,
    /// Where the AppImage was installed from, for reference.
    pub source: PathBuf,
}

/// The set of installed AppImages, backed by [`registry_path`].
#[derive(Debug, Default)]
pub struct Registry {
    entries: Vec<Entry>,
    path: PathBuf,
}

/// `$XDG_DATA_HOME/app-image-manager/installed.list`, falling back to
/// `~/.local/share/...` as the spec prescribes.
pub fn registry_path() -> io::Result<PathBuf> {
    let data_home = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is not set"))?;
    Ok(data_home.join("app-image-manager").join("installed.list"))
}

impl Registry {
    /// Read the registry, returning an empty one if it does not exist yet.
    pub fn load() -> io::Result<Self> {
        let path = registry_path()?;
        let entries = match fs::read_to_string(&path) {
            Ok(content) => parse(&content),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e),
        };
        Ok(Self { entries, path })
    }

    /// Write the registry back, creating the parent directory if needed.
    pub fn save(&self) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut f = fs::File::create(&self.path)?;
        f.write_all(serialize(&self.entries).as_bytes())
    }

    pub fn iter(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter()
    }

    #[allow(dead_code)] // used by tests and handy for inspection
    pub fn get(&self, name: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.name == name)
    }

    /// Insert `entry`, replacing any existing one with the same name.
    pub fn upsert(&mut self, entry: Entry) {
        match self.entries.iter_mut().find(|e| e.name == entry.name) {
            Some(slot) => *slot = entry,
            None => self.entries.push(entry),
        }
    }

    /// Drop the entry named `name`, returning it if it was there.
    pub fn remove(&mut self, name: &str) -> Option<Entry> {
        let idx = self.entries.iter().position(|e| e.name == name)?;
        Some(self.entries.remove(idx))
    }
}

fn parse(content: &str) -> Vec<Entry> {
    let mut out: Vec<Entry> = Vec::new();
    for raw in content.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            out.push(Entry {
                name: name.to_string(),
                ..Entry::default()
            });
            continue;
        }
        // Values belong to the group opened last; a stray line before any
        // group header is malformed, so skip it.
        let Some(current) = out.last_mut() else {
            continue;
        };
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key.trim() {
            "display" => current.display_name = value.to_string(),
            "binary" => current.binary = PathBuf::from(value),
            "desktop" => current.desktop = value.to_string(),
            "icon" => current.icon = value.to_string(),
            "source" => current.source = PathBuf::from(value),
            _ => {}
        }
    }
    out
}

fn serialize(entries: &[Entry]) -> String {
    let mut s = String::from(
        "# AppImage Manager — installed AppImages. Managed automatically;\n\
         # edit only if you know what you are doing.\n",
    );
    for e in entries {
        s.push_str(&format!("\n[{}]\n", e.name));
        s.push_str(&format!("display={}\n", e.display_name));
        s.push_str(&format!("binary={}\n", display_path(&e.binary)));
        s.push_str(&format!("desktop={}\n", e.desktop));
        s.push_str(&format!("icon={}\n", e.icon));
        s.push_str(&format!("source={}\n", display_path(&e.source)));
    }
    s
}

fn display_path(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips_entries() {
        let entries = vec![
            Entry {
                name: "zcode".into(),
                display_name: "ZCode".into(),
                binary: PathBuf::from("/home/u/.local/bin/zcode.AppImage"),
                desktop: "zcode.desktop".into(),
                icon: "zcode".into(),
                source: PathBuf::from("/home/u/dl/ZCode.AppImage"),
            },
            Entry {
                name: "other".into(),
                display_name: "Other App".into(),
                binary: PathBuf::from("/home/u/.local/bin/other.AppImage"),
                desktop: "other.desktop".into(),
                icon: "other".into(),
                source: PathBuf::from("/home/u/dl/Other.AppImage"),
            },
        ];
        let parsed = parse(&serialize(&entries));
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].name, "zcode");
        assert_eq!(parsed[0].display_name, "ZCode");
        assert_eq!(
            parsed[0].binary,
            PathBuf::from("/home/u/.local/bin/zcode.AppImage")
        );
        assert_eq!(parsed[1].desktop, "other.desktop");
    }

    #[test]
    fn parses_names_with_spaces_in_values() {
        let parsed = parse("[app]\ndisplay=My Cool App\nbinary=/x/My App.AppImage\n");
        assert_eq!(parsed[0].display_name, "My Cool App");
        assert_eq!(parsed[0].binary, PathBuf::from("/x/My App.AppImage"));
    }

    #[test]
    fn ignores_comments_and_stray_lines() {
        let parsed = parse("# comment\ndisplay=orphan\n[a]\nicon=a\nunknown=x\n");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].icon, "a");
    }

    #[test]
    fn upsert_replaces_and_remove_returns() {
        let mut reg = Registry::default();
        reg.upsert(Entry {
            name: "a".into(),
            display_name: "A".into(),
            ..Entry::default()
        });
        reg.upsert(Entry {
            name: "a".into(),
            display_name: "A2".into(),
            ..Entry::default()
        });
        assert_eq!(reg.iter().count(), 1);
        assert_eq!(reg.get("a").unwrap().display_name, "A2");
        assert!(reg.remove("a").is_some());
        assert!(reg.remove("a").is_none());
    }
}
