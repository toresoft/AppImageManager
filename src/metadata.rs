//! Targeted metadata extraction from an AppImage's squashfs payload.
//!
//! Rather than unpacking the whole filesystem (`./AppImage --appimage-extract`,
//! slow and writes a lot to disk) we extract only the files we care about via
//! `unsquashfs -o <offset> -cat <appimage> <path>`. This never executes the
//! AppImage and is fast.
//!
//! Layout (from a real sample: ZCode AppImage):
//!   /.desktop                -> the desktop entry, root of the squashfs
//!   /.DirIcon                -> symlink/icon fallback
//!   /usr/share/icons/hicolor/<size>/apps/<name>.png  -> icon variants
//!
//! Note: `unsquashfs` lives in `/usr/sbin` here, so we resolve via PATH.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::appimage::AppImage;
use crate::desktop::DesktopEntry;

/// An extracted icon: its size in pixels and the PNG bytes.
#[derive(Debug, Clone)]
pub struct Icon {
    pub size: u32,
    pub png: Vec<u8>,
}

/// Metadata extracted from an AppImage for installation.
#[derive(Debug, Clone)]
pub struct AppImageMetadata {
    /// The desktop entry that ships inside the AppImage.
    pub desktop: DesktopEntry,
    /// All hicolor PNG icons found under `/usr/share/icons/hicolor/*/apps/`.
    pub icons: Vec<Icon>,
    /// The `.DirIcon` bytes (PNG), if present — used as a fallback.
    pub dir_icon: Option<Vec<u8>>,
}

#[derive(Debug)]
pub enum MetadataError {
    /// `unsquashfs` not found on PATH.
    UnsquashfsNotFound,
    /// `unsquashfs` ran but failed (bad offset, missing file inside, ...).
    Extract(String),
    /// The AppImage has no `.desktop` file in its root.
    NoDesktopEntry,
    /// The desktop entry is missing the minimum required keys.
    InvalidDesktopEntry(String),
    Io(std::io::Error),
}

impl std::fmt::Display for MetadataError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MetadataError::UnsquashfsNotFound => {
                write!(f, "unsquashfs not found on PATH")
            }
            MetadataError::Extract(s) => write!(f, "extraction failed: {s}"),
            MetadataError::NoDesktopEntry => {
                write!(f, "no .desktop file at the AppImage root")
            }
            MetadataError::InvalidDesktopEntry(s) => {
                write!(f, "invalid desktop entry: {s}")
            }
            MetadataError::Io(e) => write!(f, "I/O error: {e}"),
        }
    }
}

impl std::error::Error for MetadataError {}

impl From<std::io::Error> for MetadataError {
    fn from(e: std::io::Error) -> Self {
        if e.kind() == std::io::ErrorKind::NotFound {
            MetadataError::UnsquashfsNotFound
        } else {
            MetadataError::Io(e)
        }
    }
}

/// Extract a single file from the AppImage payload as bytes.
///
/// Returns `Ok(None)` if the path does not exist inside the squashfs
/// (so callers can treat optional files like `.DirIcon` gracefully).
fn extract_file(
    app: &AppImage,
    appimage: &Path,
    inner_path: &str,
) -> Result<Option<Vec<u8>>, MetadataError> {
    let output = Command::new("unsquashfs")
        .args([
            "-o",
            &app.squashfs_offset.to_string(),
            "-cat",
            &appimage.to_string_lossy(),
            inner_path,
        ])
        .output()?;

    // unsquashfs returns non-zero when the path is missing. Distinguish
    // "missing" (we tolerate for optional files) from real failures by exit
    // code: code 1 with stderr containing "not found" / "No such file" => None.
    // We avoid string-matching stderr (locale); instead, treat code != 0 with
    // empty stdout as "missing", and non-empty stderr as a real error.
    if output.status.success() {
        return Ok(Some(output.stdout));
    }
    if output.stdout.is_empty() {
        // Likely missing path. Surface stderr only if it indicates a non-404
        // problem: if stderr is empty too, treat as missing.
        if output.stderr.is_empty() {
            return Ok(None);
        }
        // Heuristic: real errors usually mention the file we asked for.
        // We accept missing-file as None and propagate the rest.
        let err = String::from_utf8_lossy(&output.stderr);
        let lower = err.to_ascii_lowercase();
        if lower.contains("not found")
            || lower.contains("no such")
            || lower.contains("does not exist")
            || lower.contains("nonexistent")
            || lower.contains("not exist")
        {
            return Ok(None);
        }
        return Err(MetadataError::Extract(format!("unsquashfs: {err}")));
    }
    Ok(Some(output.stdout))
}

impl AppImageMetadata {
    /// Extract metadata from `appimage` (path on disk) using the validated
    /// squashfs offset in `app`.
    pub fn extract(appimage: &Path, app: &AppImage) -> Result<Self, MetadataError> {
        let desktop = Self::extract_desktop(appimage, app)?;
        let icons = Self::extract_icons(appimage, app)?;
        let dir_icon = extract_file(app, appimage, ".DirIcon")?;
        Ok(Self {
            desktop,
            icons,
            dir_icon,
        })
    }

    fn extract_desktop(appimage: &Path, app: &AppImage) -> Result<DesktopEntry, MetadataError> {
        // The desktop entry is a `*.desktop` file at the squashfs root. We
        // don't know its exact name ahead of time, but it matches the app's
        // name. Common conventions: `<name>.desktop`. We list the root to
        // find it rather than guessing.
        let desktop_name = find_root_desktop_name(appimage, app)?;
        let bytes =
            extract_file(app, appimage, &desktop_name)?.ok_or(MetadataError::NoDesktopEntry)?;
        let content = String::from_utf8_lossy(&bytes);
        let entry = DesktopEntry::parse(&content);

        // Validate the minimum required keys.
        if entry.get("Name").is_none() {
            return Err(MetadataError::InvalidDesktopEntry(
                "missing Name".to_string(),
            ));
        }
        if entry.get("Type").unwrap_or("Application") != "Application" {
            // Allow, but warn via error if it's clearly not an app.
        }
        Ok(entry)
    }

    fn extract_icons(appimage: &Path, app: &AppImage) -> Result<Vec<Icon>, MetadataError> {
        let listing = list_dir(appimage, app, "usr/share/icons/hicolor")?;
        let mut icons = Vec::new();
        for entry in listing.lines() {
            // Expect entries like `usr/share/icons/hicolor/48x48/apps/<name>.png`
            let parts: Vec<&str> = entry.split('/').collect();
            // Index layout: usr/share/icons/hicolor/<SIZE>/apps/<NAME>.png
            if parts.len() < 7 {
                continue;
            }
            if parts[5] != "apps" {
                continue;
            }
            let size_str = parts[4];
            let file = parts[6];
            if !file.ends_with(".png") {
                continue;
            }
            let Some(size) = size_str.split('x').next() else {
                continue;
            };
            let Ok(size) = size.parse::<u32>() else {
                continue;
            };
            let path = format!("usr/share/icons/hicolor/{size_str}/apps/{file}");
            if let Some(png) = extract_file(app, appimage, &path)? {
                icons.push(Icon { size, png });
            }
        }
        Ok(icons)
    }
}

/// List the contents of the squashfs (paths only) under `root` via
/// `unsquashfs -o <offset> -ls <appimage> <root>`.
///
/// `-ls` prints one path per line (with a `squashfs-root/` prefix); this is
/// far more robust to parse than the long `-ll` format (which embeds
/// timestamps/permissions whose layout varies by locale and unsquashfs
/// version).
fn list_dir(appimage: &Path, app: &AppImage, root: &str) -> Result<String, MetadataError> {
    let output = Command::new("unsquashfs")
        .args([
            "-o",
            &app.squashfs_offset.to_string(),
            "-ls",
            &appimage.to_string_lossy(),
            root,
        ])
        .output()?;

    if !output.status.success() && output.stdout.is_empty() {
        let err = String::from_utf8_lossy(&output.stderr);
        return Err(MetadataError::Extract(format!("unsquashfs -ls: {err}")));
    }
    let raw = String::from_utf8_lossy(&output.stdout);
    Ok(parse_ls(&raw, root))
}

/// Parse `unsquashfs -ls` output into a newline-separated list of paths
/// (without the `squashfs-root/` prefix), filtered to those under `root`.
fn parse_ls(raw: &str, root: &str) -> String {
    let mut paths = Vec::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed == "squashfs-root" {
            continue;
        }
        let cleaned = trimmed
            .trim_start_matches("squashfs-root/")
            .trim_start_matches("./");
        if cleaned.starts_with(root) {
            paths.push(cleaned.to_string());
        }
    }
    paths.join("\n")
}

/// Find the `.desktop` filename at the squashfs root.
fn find_root_desktop_name(appimage: &Path, app: &AppImage) -> Result<String, MetadataError> {
    // unsquashfs -ls lists the root directory contents.
    let output = Command::new("unsquashfs")
        .args([
            "-o",
            &app.squashfs_offset.to_string(),
            "-ls",
            &appimage.to_string_lossy(),
        ])
        .output()?;

    let raw = String::from_utf8_lossy(&output.stdout);
    for line in raw.lines() {
        let t = line.trim();
        // Root listing prints entries like "Foo.desktop" possibly prefixed by
        // a pseudo-root path. Match any token ending in `.desktop`.
        for token in t.split_whitespace() {
            let cleaned = token
                .trim_start_matches("squashfs-root/")
                .trim_start_matches("./");
            if cleaned.ends_with(".desktop") && !cleaned.contains('/') {
                return Ok(cleaned.to_string());
            }
        }
    }
    Err(MetadataError::NoDesktopEntry)
}

/// Convenience: derive a stable install name for an AppImage.
///
/// We prefer the `Name` from the desktop entry (lower-cased, ASCII-only),
/// falling back to the file stem.
pub fn install_name(desktop: &DesktopEntry, file: &Path) -> String {
    if let Some(name) = desktop.get("Name") {
        let cleaned: String = name
            .chars()
            .map(|c| c.to_ascii_lowercase())
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .collect();
        if !cleaned.is_empty() {
            return cleaned;
        }
    }
    file.file_stem()
        .map(PathBuf::from)
        .and_then(|s| s.to_str().map(str::to_string))
        .unwrap_or_else(|| "appimage-app".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ls_strips_prefix_and_filters() {
        let raw = "\
squashfs-root
squashfs-root/usr
squashfs-root/usr/share/icons/hicolor/48x48/apps/zcode.png
squashfs-root/usr/share/icons/hicolor/128x128/apps/zcode.png
squashfs-root/something/else
";
        let out = parse_ls(raw, "usr/share/icons/hicolor");
        assert_eq!(
            out,
            "usr/share/icons/hicolor/48x48/apps/zcode.png\n\
             usr/share/icons/hicolor/128x128/apps/zcode.png"
        );
    }

    #[test]
    fn install_name_uses_desktop_name() {
        let mut d = DesktopEntry::default();
        d.set("Name", "My Cool App");
        let name = install_name(&d, Path::new("/tmp/whatever.AppImage"));
        assert_eq!(name, "mycoolapp");
    }
}
