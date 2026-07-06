//! Installation logic: copy the AppImage, install icons, write the
//! rewritten `.desktop`, refresh KDE/XDG caches.
//!
//! Scope is per-user only: everything lives under `$HOME/.local`.

use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::appimage::{AppImage, AppImageError};
use crate::desktop::DesktopEntry;
use crate::metadata::{AppImageMetadata, MetadataError, install_name};

/// Marker we add to generated desktop entries so `list`/`uninstall` can find
/// them and so we never touch unrelated entries.
pub const MARKER_KEY: &str = "X-AppImage-Manager";

/// Prefix used for the generated `.desktop` filenames.
///
/// AppImages expose their own `.desktop` from their mount point, and KDE
/// Plasma deduplicates entries by filename. Namespacing ours avoids the
/// collision that would otherwise hide our entry.
pub const DESKTOP_PREFIX: &str = "appimage-manager-";

/// Build the `.desktop` filename for a logical install `name`.
fn desktop_file_name(name: &str) -> String {
    format!("{DESKTOP_PREFIX}{name}.desktop")
}

/// Reverse of [`desktop_file_name`]: extract the logical `name` from a
/// generated `.desktop` filename. Returns `None` if the file is not one of
/// ours (wrong prefix / extension).
fn name_from_desktop_file(file: &str) -> Option<String> {
    let stem = file.strip_suffix(".desktop")?;
    stem.strip_prefix(DESKTOP_PREFIX).map(str::to_string)
}

#[derive(Debug)]
pub enum InstallError {
    AppImage(AppImageError),
    Metadata(MetadataError),
    Io(io::Error),
    /// A required helper binary was missing.
    #[allow(dead_code)]
    HelperMissing(String),
    /// A helper ran but failed.
    #[allow(dead_code)]
    HelperFailed(String, String),
}

impl std::fmt::Display for InstallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InstallError::AppImage(e) => write!(f, "{e}"),
            InstallError::Metadata(e) => write!(f, "{e}"),
            InstallError::Io(e) => write!(f, "I/O error: {e}"),
            InstallError::HelperMissing(n) => write!(f, "helper not found: {n}"),
            InstallError::HelperFailed(n, m) => {
                write!(f, "helper {n} failed: {m}")
            }
        }
    }
}

impl std::error::Error for InstallError {}

impl From<AppImageError> for InstallError {
    fn from(e: AppImageError) -> Self {
        InstallError::AppImage(e)
    }
}
impl From<MetadataError> for InstallError {
    fn from(e: MetadataError) -> Self {
        InstallError::Metadata(e)
    }
}
impl From<io::Error> for InstallError {
    fn from(e: io::Error) -> Self {
        InstallError::Io(e)
    }
}

/// Result of a successful installation.
#[derive(Debug, Clone)]
pub struct InstalledApp {
    /// Canonical name (e.g. `zcode`).
    pub name: String,
    /// Human-readable name from the desktop entry.
    pub display_name: String,
    /// Path where the AppImage executable was copied.
    pub binary: PathBuf,
    /// Path of the generated `.desktop` file.
    #[allow(dead_code)]
    pub desktop: PathBuf,
    /// Non-empty when KDE may not see the menu entry because `~/.local/share`
    /// is missing from `XDG_DATA_DIRS`. Carries a user-facing hint.
    #[allow(dead_code)]
    pub xdg_warning: Option<String>,
}

/// Where per-user integration files live.
pub struct Dirs {
    pub bin: PathBuf,
    pub applications: PathBuf,
}

impl Dirs {
    /// Resolve from `$HOME/.local/{bin,share/applications}`, creating them.
    pub fn ensure() -> io::Result<Self> {
        let home = std::env::var_os("HOME")
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is not set"))?;
        let local = PathBuf::from(home).join(".local");
        let bin = local.join("bin");
        let applications = local.join("share").join("applications");
        fs::create_dir_all(&bin)?;
        fs::create_dir_all(&applications)?;
        Ok(Self { bin, applications })
    }
}

/// Top-level install entry point.
pub fn install(appimage: &Path) -> Result<InstalledApp, InstallError> {
    let canonical = fs::canonicalize(appimage)?;
    let app = AppImage::open(&canonical)?;
    let meta = AppImageMetadata::extract(&canonical, &app)?;
    install_from_metadata(&canonical, &app, meta)
}

/// Install from already-extracted metadata (lets us reuse the metadata
/// extraction in tests and avoid re-reading).
fn install_from_metadata(
    appimage: &Path,
    _app: &AppImage,
    meta: AppImageMetadata,
) -> Result<InstalledApp, InstallError> {
    let dirs = Dirs::ensure()?;
    let name = install_name(&meta.desktop, appimage);
    let display_name = meta
        .desktop
        .get("Name")
        .map(str::to_string)
        .unwrap_or_else(|| name.clone());

    // 1. Copy the AppImage binary to ~/.local/bin/<name>.AppImage
    let bin_name = format!("{name}.AppImage");
    let bin_path = dirs.bin.join(&bin_name);
    copy_executable(appimage, &bin_path)?;

    // 2. Rewrite the .desktop entry.
    //
    // The .desktop FILENAME is given a stable, namespaced prefix so it can
    // never collide with the .desktop entry an AppImage exposes from its own
    // mount point (/tmp/.mount_<app>/usr/share/applications/<name>.desktop).
    // KDE Plasma deduplicates entries by filename: if our `zcode.desktop` in
    // ~/.local/share/applications clashes with the mounted one, Plasma hides
    // ours. A namespaced name (`appimage-manager-zcode.desktop`) sidesteps
    // that and always shows up.
    let desktop_path = dirs.applications.join(desktop_file_name(&name));
    let desktop = rewrite_desktop(&meta.desktop, &bin_path, &name, appimage, &display_name);

    // 3. Install icons (hicolor) before writing the .desktop so the Icon=
    // name resolves immediately.
    install_icons(&name, &meta);

    // 4. Write the .desktop file.
    {
        let mut f = fs::File::create(&desktop_path)?;
        f.write_all(desktop.to_string().as_bytes())?;
    }

    // 5. Refresh XDG caches (best-effort; helpers may be absent on minimal
    // installs, in which case we proceed).
    let _ = run_helper("update-desktop-database", [dirs.applications.as_os_str()]);
    let _ = refresh_icon_cache();
    // KDE reads .desktop entries only from the directories listed in
    // XDG_DATA_DIRS (plus the system default). If ~/.local/share is missing
    // from it, the menu entry we just wrote would be invisible until the user
    // fixes their environment. We rebuild the KDE sycoca cache with the
    // correct path so the entry shows up immediately.
    let xdg_warning = ensure_kde_sees_user_applications(&dirs);

    Ok(InstalledApp {
        name,
        display_name,
        binary: bin_path,
        desktop: desktop_path,
        xdg_warning,
    })
}

/// Copy `src` to `dst`, ensuring the destination is executable (0700) and
/// not a symlink to something we'd race with.
fn copy_executable(src: &Path, dst: &Path) -> Result<(), InstallError> {
    // AppImages are regular executables. Verify it's a regular file.
    let meta = fs::metadata(src)?;
    if !meta.is_file() {
        return Err(InstallError::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            "source is not a regular file",
        )));
    }
    let _ = meta.file_type().is_block_device(); // touch to silence warnings
    // Remove an existing destination so the copy is clean.
    if dst.exists() {
        fs::remove_file(dst)?;
    }
    fs::copy(src, dst)?;
    let mut perms = fs::metadata(dst)?.permissions();
    perms.set_mode(0o700);
    fs::set_permissions(dst, perms)?;
    Ok(())
}

/// Build the rewritten desktop entry for the installed AppImage.
fn rewrite_desktop(
    src: &DesktopEntry,
    bin_path: &Path,
    icon_name: &str,
    source_path: &Path,
    display_name: &str,
) -> DesktopEntry {
    let mut d = src.clone();

    // Replace the relative `AppRun`-based Exec with an absolute path.
    if let Some(exec) = d.get("Exec").map(str::to_string) {
        d.set("Exec", &rewrite_exec(&exec, bin_path));
    } else {
        // Some entries omit Exec; provide a sane default.
        d.set("Exec", &format!("{} %U", bin_path.display()));
    }
    // Force a stable icon name so we control the icon set we installed.
    d.set("Icon", icon_name);
    // Make sure Name is set (we validated it in metadata extraction, but be
    // defensive in case the upstream entry used a locale key only).
    if d.get("Name").is_none() {
        d.set("Name", display_name);
    }
    // Markers so we can list/uninstall our entries only.
    d.set(MARKER_KEY, "true");
    d.set("X-AppImage-Source", &source_path.to_string_lossy());
    // `TryExec` would hide the entry if the binary isn't executable; keep it
    // only if it currently points somewhere sensible, otherwise drop it to
    // avoid the entry being masked.
    if d.get("TryExec").is_some() {
        d.remove("TryExec");
        d.set("TryExec", &bin_path.to_string_lossy());
    }

    d
}

/// Turn an upstream `Exec=AppRun <args> %U` into `Exec=<abs binary> <args> %U`.
///
/// The first whitespace-separated token is the program name (`AppRun` or
/// occasionally an absolute path); we replace just that token, preserving
/// every argument that follows.
fn rewrite_exec(exec: &str, bin_path: &Path) -> String {
    let bin = bin_path.to_string_lossy();
    match exec.split_once(char::is_whitespace) {
        Some((_old_prog, args)) => {
            let args = args.trim_start();
            if args.is_empty() {
                bin.into_owned()
            } else {
                format!("{bin} {args}")
            }
        }
        None => bin.into_owned(),
    }
}

/// Install all extracted PNG icons under the hicolor theme using
/// `xdg-icon-resource`, falling back to a manual copy.
fn install_icons(icon_name: &str, meta: &AppImageMetadata) {
    let fallback = meta.dir_icon.as_deref();
    // Install every shipped icon size. (Do not short-circuit: `.any()` would
    // stop after the first success and leave the other sizes uninstalled.)
    let mut used_any = false;
    for ic in &meta.icons {
        if install_one_icon(icon_name, ic.size, &ic.png).is_ok() {
            used_any = true;
        }
    }

    // If no themed icons were shipped, drop the `.DirIcon` as a 512px icon.
    if !used_any && let Some(png) = fallback {
        let _ = install_one_icon(icon_name, 512, png);
    }
}

/// Install a single PNG via `xdg-icon-resource`, with a manual-copy fallback.
fn install_one_icon(name: &str, size: u32, png: &[u8]) -> io::Result<()> {
    // Write the PNG to a temp file so xdg-icon-resource can read it.
    let tmp = temp_icon_path(size)?;
    fs::write(&tmp, png)?;
    let res = Command::new("xdg-icon-resource")
        .args([
            "install",
            "--noupdate",
            "--novendor",
            "--size",
            &size.to_string(),
            &tmp.to_string_lossy(),
            name,
        ])
        .status();
    let _ = fs::remove_file(&tmp);
    match res {
        Ok(s) if s.success() => Ok(()),
        _ => {
            // Fallback: copy into ~/.local/share/icons/hicolor/<size>x<size>/apps/
            manual_install_icon(name, size, png)
        }
    }
}

fn manual_install_icon(name: &str, size: u32, png: &[u8]) -> io::Result<()> {
    let Some(home) = std::env::var_os("HOME") else {
        return Err(io::Error::new(io::ErrorKind::NotFound, "HOME unset"));
    };
    let dir = PathBuf::from(home)
        .join(".local/share/icons/hicolor")
        .join(format!("{size}x{size}/apps"));
    fs::create_dir_all(&dir)?;
    let dst = dir.join(format!("{name}.png"));
    fs::write(dst, png)?;
    Ok(())
}

fn temp_icon_path(size: u32) -> io::Result<PathBuf> {
    let dir = std::env::temp_dir();
    let pid = std::process::id();
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    Ok(dir.join(format!("app-image-manager-icon-{size}-{pid}-{ts}.png")))
}

/// Run a helper, returning stderr text on failure.
fn run_helper<I, S>(name: &str, args: I) -> Result<(), (String, String)>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let output = Command::new(name)
        .args(args)
        .output()
        .map_err(|e| (name.to_string(), e.to_string()))?;
    if output.status.success() {
        Ok(())
    } else {
        let err = String::from_utf8_lossy(&output.stderr).to_string();
        Err((name.to_string(), err))
    }
}

fn refresh_icon_cache() -> Result<(), (String, String)> {
    // gtk-update-icon-cache works on theme dirs; for hicolor user dir:
    let Some(home) = std::env::var_os("HOME") else {
        return Err(("HOME".into(), "unset".into()));
    };
    let theme_dir = PathBuf::from(home).join(".local/share/icons/hicolor");
    run_helper("gtk-update-icon-cache", [theme_dir.as_os_str()])
}

/// Make sure KDE will actually pick up the `.desktop` we wrote under
/// `~/.local/share/applications`.
///
/// KDE Plasma only scans the directories listed in `XDG_DATA_DIRS` (plus the
/// compiled-in system default) when building its menu cache (ksycoca). The
/// freedesktop spec says `~/.local/share` should be consulted regardless, but
/// in practice some environments — notably when an AppImage prepends its own
/// `usr/share` to `XDG_DATA_DIRS` — end up without `~/.local/share` in the
/// list, and the menu entry we just created stays invisible.
///
/// This function detects that situation and, as a remedy, rebuilds the KDE
/// sycoca cache with `~/.local/share` explicitly prepended to `XDG_DATA_DIRS`,
/// so the entry appears immediately. It also returns a user-facing hint when
/// the environment needs a permanent fix.
fn ensure_kde_sees_user_applications(dirs: &Dirs) -> Option<String> {
    let local_share = dirs
        .applications
        .parent()
        .expect("applications has no parent");
    let local_share_str = local_share.to_string_lossy().to_string();

    let in_xdg = std::env::var_os("XDG_DATA_DIRS")
        .map(|v| v.to_string_lossy().split(':').any(|p| p == local_share_str))
        .unwrap_or(false);

    if in_xdg {
        // Environment is fine: just refresh the cache normally.
        rebuild_kde_sycoca(None);
        return None;
    }

    // `~/.local/share` is NOT in XDG_DATA_DIRS. Rebuild the cache with it
    // prepended so our entry is picked up for the current session.
    let new_xdg = format!(
        "{local_share_str}:{}",
        std::env::var("XDG_DATA_DIRS").unwrap_or_default()
    );
    rebuild_kde_sycoca(Some(&new_xdg));

    Some(format!(
        "KDE potrebbe non mostrare la voce di menù perché «{path}» non è in XDG_DATA_DIRS.\n\
         Per renderlo permanente, aggiungi questa riga a ~/.bash_profile o ~/.profile:\n\n\
         export XDG_DATA_DIRS=\"$HOME/.local/share:$XDG_DATA_DIRS\"\n\n\
         (La voce è già visibile nella sessione corrente.)",
        path = local_share_str
    ))
}

/// Rebuild the KDE service-type cache (ksycoca), optionally overriding
/// `XDG_DATA_DIRS` for the rebuild. Best-effort: silently ignores missing
/// `kbuildsycoca` (non-KDE systems) or failures.
fn rebuild_kde_sycoca(xdg_override: Option<&str>) {
    for bin in ["kbuildsycoca6", "kbuildsycoca5"] {
        let mut cmd = Command::new(bin);
        cmd.arg("--noincremental");
        if let Some(xdg) = xdg_override {
            cmd.env("XDG_DATA_DIRS", xdg);
        }
        // Suppress output; this is best-effort.
        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        if cmd.status().is_ok() {
            return;
        }
    }
}

/// List installed AppImages (those whose .desktop has our marker).
pub fn list() -> io::Result<Vec<InstalledApp>> {
    let dirs = Dirs::ensure()?;
    let mut out = Vec::new();
    if !dirs.applications.exists() {
        return Ok(out);
    }
    for entry in fs::read_dir(dirs.applications)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("desktop") {
            continue;
        }
        let Ok(content) = fs::read_to_string(&path) else {
            continue;
        };
        let d = DesktopEntry::parse(&content);
        if d.get(MARKER_KEY) != Some("true") {
            continue;
        }
        // Derive the logical name by stripping our namespace prefix from the
        // filename (not the Name field, which may be localized or spaced).
        let file_name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        let name = name_from_desktop_file(file_name).unwrap_or_else(|| file_name.to_string());
        let display_name = d.get("Name").unwrap_or(&name).to_string();
        let binary = d
            .get("Exec")
            .map(|e| e.split_whitespace().next().unwrap_or("").to_string())
            .map(PathBuf::from)
            .unwrap_or_default();
        out.push(InstalledApp {
            name,
            display_name,
            binary,
            desktop: path,
            xdg_warning: None,
        });
    }
    Ok(out)
}

/// Uninstall by name. Returns true if something was removed.
pub fn uninstall(name: &str) -> Result<bool, InstallError> {
    let dirs = Dirs::ensure()?;
    let desktop_path = dirs.applications.join(desktop_file_name(name));
    if !desktop_path.exists() {
        return Ok(false);
    }
    let content = fs::read_to_string(&desktop_path)?;
    let d = DesktopEntry::parse(&content);

    // Remove the binary.
    if let Some(bin) = d.get("Exec").and_then(|e| e.split_whitespace().next()) {
        let bin = PathBuf::from(bin);
        if bin.starts_with(&dirs.bin) && bin.exists() {
            let _ = fs::remove_file(&bin);
        }
    }
    // Remove icons across common sizes.
    if let Some(icon) = d.get("Icon") {
        uninstall_icons(icon);
    }
    // Remove the desktop entry itself.
    fs::remove_file(&desktop_path)?;

    let _ = run_helper("update-desktop-database", [dirs.applications.as_os_str()]);
    let _ = refresh_icon_cache();
    Ok(true)
}

fn uninstall_icons(name: &str) {
    for size in [16, 22, 24, 32, 48, 64, 128, 256, 512, 1024] {
        let _ = Command::new("xdg-icon-resource")
            .args(["uninstall", "--size", &size.to_string(), name])
            .status();
        // Manual fallback removal too.
        if let Some(home) = std::env::var_os("HOME") {
            let p = PathBuf::from(home)
                .join(".local/share/icons/hicolor")
                .join(format!("{size}x{size}/apps"))
                .join(format!("{name}.png"));
            let _ = fs::remove_file(p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_apprun_exec() {
        let new = rewrite_exec(
            "AppRun --no-sandbox %U",
            Path::new("/home/u/.local/bin/zcode.AppImage"),
        );
        assert_eq!(new, "/home/u/.local/bin/zcode.AppImage --no-sandbox %U");
    }

    #[test]
    fn rewrites_exec_without_args() {
        let new = rewrite_exec("AppRun", Path::new("/x/y.AppImage"));
        assert_eq!(new, "/x/y.AppImage");
    }

    #[test]
    fn rewrite_desktop_sets_marker_and_icon() {
        let mut src = DesktopEntry::default();
        src.set("Name", "ZCode");
        src.set("Exec", "AppRun --no-sandbox %U");
        src.set("Icon", "zcode");
        let out = rewrite_desktop(
            &src,
            Path::new("/home/u/.local/bin/zcode.AppImage"),
            "zcode",
            Path::new("/home/u/dl/ZCode.AppImage"),
            "ZCode",
        );
        assert_eq!(out.get("Icon"), Some("zcode"));
        assert_eq!(
            out.get("Exec"),
            Some("/home/u/.local/bin/zcode.AppImage --no-sandbox %U")
        );
        assert_eq!(out.get(MARKER_KEY), Some("true"));
        assert!(out.get("X-AppImage-Source").is_some());
    }

    #[test]
    fn desktop_file_name_roundtrip() {
        let f = desktop_file_name("zcode");
        assert_eq!(f, "appimage-manager-zcode.desktop");
        assert_eq!(name_from_desktop_file(&f).as_deref(), Some("zcode"));
    }

    #[test]
    fn name_from_desktop_file_rejects_foreign() {
        // Files without our prefix or extension must not parse as ours.
        assert_eq!(name_from_desktop_file("zcode.desktop"), None);
        assert_eq!(name_from_desktop_file("appimage-manager-zcode"), None);
        assert_eq!(name_from_desktop_file("appimage-manager-zcode.png"), None);
    }
}
