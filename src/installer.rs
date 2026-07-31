//! Installation logic: copy the AppImage, install icons, write the
//! rewritten `.desktop`, refresh KDE/XDG caches.
//!
//! Scope is per-user only: everything lives under `$HOME/.local`.

use std::collections::HashSet;
use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

use rust_i18n::t;

use crate::appimage::{AppImage, AppImageError};
use crate::desktop::DesktopEntry;
use crate::metadata::{AppImageMetadata, MetadataError, install_name};
use crate::registry::{Entry, Registry};

/// Marker we add to generated desktop entries. It is informational (and used
/// to recognise entries written by ≤0.1.2); the authoritative record of what
/// we installed is [`crate::registry`].
pub const MARKER_KEY: &str = "X-AppImage-Manager";

/// Prefix 0.1.2 gave to the generated `.desktop` filenames.
///
/// It was meant to avoid a filename collision, but it changed the *desktop
/// file ID*, which is what actually caused duplicate menu entries: an
/// application that registers itself (Electron does this from
/// `setAsDefaultProtocolClient`) writes `<name>.desktop`, and with ours living
/// under a different ID the two coexisted instead of one replacing the other.
/// We only keep the prefix to migrate away from it.
const LEGACY_DESKTOP_PREFIX: &str = "appimage-manager-";

/// Build the `.desktop` filename for a logical install `name`.
///
/// This is the *canonical* ID — the same one the application would pick for
/// itself — so a self-registering app overwrites our entry rather than adding
/// a second one. Losing our keys that way is harmless: the registry, not the
/// `.desktop`, tracks the install.
fn desktop_file_name(name: &str) -> String {
    format!("{name}.desktop")
}

/// Extract the logical `name` from a `.desktop` filename written by ≤0.1.2.
/// Returns `None` if the file is not one of those (wrong prefix / extension).
fn name_from_legacy_desktop_file(file: &str) -> Option<String> {
    let stem = file.strip_suffix(".desktop")?;
    stem.strip_prefix(LEGACY_DESKTOP_PREFIX).map(str::to_string)
}

/// Extract the program from an `Exec=` value.
///
/// Per the spec the value is a quoted-argument list: a field may be wrapped in
/// double quotes (necessary when the path contains spaces) with `\` escaping
/// inside. Applications that self-register routinely emit the quoted form —
/// `Exec="/home/u/.local/bin/zcode.AppImage" %U` — so a naive
/// `split_whitespace().next()` would yield a path with a stray quote and fail
/// to match ours.
fn exec_program(exec: &str) -> Option<String> {
    let s = exec.trim_start();
    let Some(quoted) = s.strip_prefix('"') else {
        return s.split_whitespace().next().map(str::to_string);
    };
    let mut out = String::new();
    let mut chars = quoted.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => out.push(chars.next()?),
            _ => out.push(c),
        }
    }
    // Unterminated quote: take what we got rather than nothing.
    Some(out)
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

    // Open the registry first: it may migrate a ≤0.1.2 entry for this very
    // app, and that migration writes the canonical `.desktop`. Doing it before
    // step 4 makes sure the entry we are about to write wins.
    let (mut reg, _) = open_registry(&dirs)?;

    // 1. Copy the AppImage binary to ~/.local/bin/<name>.AppImage
    let bin_name = format!("{name}.AppImage");
    let bin_path = dirs.bin.join(&bin_name);
    copy_executable(appimage, &bin_path)?;

    // 2. Rewrite the .desktop entry, under the canonical file ID (see
    // `desktop_file_name`).
    let desktop_file = desktop_file_name(&name);
    let desktop_path = dirs.applications.join(&desktop_file);
    let desktop = rewrite_desktop(&meta.desktop, &bin_path, &name, appimage, &display_name);

    // 3. Install icons (hicolor) before writing the .desktop so the Icon=
    // name resolves immediately.
    install_icons(&name, &meta);

    // 4. Write the .desktop file.
    {
        let mut f = fs::File::create(&desktop_path)?;
        f.write_all(desktop.to_string().as_bytes())?;
    }

    // 5. Record the install, then drop any menu entry that duplicates it.
    reg.upsert(Entry {
        name: name.clone(),
        display_name: display_name.clone(),
        binary: bin_path.clone(),
        desktop: desktop_file,
        icon: name.clone(),
        source: appimage.to_path_buf(),
    });
    reg.save()?;
    prune_duplicate_entries(&dirs, &reg);

    // 6. Refresh XDG caches (best-effort; helpers may be absent on minimal
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
    // Action groups (`[Desktop Action <id>]`) carry their own Exec, pointing
    // at the same relative `AppRun`. Rewrite those too, otherwise the actions
    // KDE shows in the launcher context menu would fail to start anything.
    for group in &mut d.groups {
        for (key, value) in &mut group.keys {
            if key == "Exec" {
                *value = rewrite_exec(value, bin_path);
            }
        }
    }

    // Force a stable icon name so we control the icon set we installed.
    d.set("Icon", icon_name);
    // Make sure Name is set (we validated it in metadata extraction, but be
    // defensive in case the upstream entry used a locale key only).
    if d.get("Name").is_none() {
        d.set("Name", display_name);
    }
    // `Type` is required by the spec; an entry without it is invalid and gets
    // ignored. Upstream entries virtually always have it, but don't rely on it.
    if d.get("Type").is_none() {
        d.set("Type", "Application");
    }
    // `Hidden=true` means "this entry has been deleted" to every XDG
    // implementation — we are deliberately (re)creating it.
    d.remove("Hidden");
    // Markers so an entry can be recognised as ours on inspection.
    d.set(MARKER_KEY, "true");
    d.set("X-AppImage-Source", &source_path.to_string_lossy());
    // `TryExec` would hide the entry if it pointed at a binary that is not
    // executable; repoint it at the copy we just installed.
    if d.get("TryExec").is_some() {
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
    // `xdg-icon-resource forceupdate` is the spec-blessed way to regenerate the
    // theme cache and knows how to set up the theme directory correctly.
    if run_helper("xdg-icon-resource", ["forceupdate", "--theme", "hicolor"]).is_ok() {
        return Ok(());
    }
    // Fallback: drive gtk-update-icon-cache directly. `--ignore-theme-index` is
    // required because the per-user hicolor directory usually has no
    // `index.theme` of its own (it inherits the system one), and without the
    // flag the tool refuses to build the cache.
    let Some(home) = std::env::var_os("HOME") else {
        return Err(("HOME".into(), "unset".into()));
    };
    let theme_dir = PathBuf::from(home).join(".local/share/icons/hicolor");
    use std::ffi::OsStr;
    run_helper(
        "gtk-update-icon-cache",
        [
            OsStr::new("--force"),
            OsStr::new("--ignore-theme-index"),
            OsStr::new("--quiet"),
            theme_dir.as_os_str(),
        ],
    )
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

    Some(t!("xdg_warning", path = local_share_str).to_string())
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

/// Load the registry, first importing anything installed by ≤0.1.2.
/// The flag reports whether the migration changed anything on disk.
fn open_registry(dirs: &Dirs) -> io::Result<(Registry, bool)> {
    let mut reg = Registry::load()?;
    let migrated = migrate_legacy_entries(dirs, &mut reg);
    if migrated {
        reg.save()?;
    }
    Ok((reg, migrated))
}

/// Tell the desktop environment to re-read the menu.
fn refresh_menu(dirs: &Dirs) {
    let _ = run_helper("update-desktop-database", [dirs.applications.as_os_str()]);
    rebuild_kde_sycoca(None);
}

/// Import installs made by ≤0.1.2 — which recorded everything in a
/// `appimage-manager-<name>.desktop` file — into the registry, and move their
/// menu entry to the canonical file ID.
///
/// Rewriting under the canonical ID is what removes the duplicate for existing
/// users: it lands on (and replaces) the `<name>.desktop` the application
/// registered for itself, collapsing the two menu entries back into one.
/// Returns whether anything changed.
fn migrate_legacy_entries(dirs: &Dirs, reg: &mut Registry) -> bool {
    let Ok(read_dir) = fs::read_dir(&dirs.applications) else {
        return false;
    };
    let mut changed = false;
    for dir_entry in read_dir.flatten() {
        let path = dir_entry.path();
        let file_name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        let Some(name) = name_from_legacy_desktop_file(file_name) else {
            continue;
        };
        let Ok(content) = fs::read_to_string(&path) else {
            continue;
        };
        let d = DesktopEntry::parse(&content);
        if d.get(MARKER_KEY) != Some("true") {
            // Not ours despite the prefix — leave it alone.
            continue;
        }
        let desktop_file = desktop_file_name(&name);
        if fs::write(dirs.applications.join(&desktop_file), &content).is_err() {
            continue;
        }
        let _ = fs::remove_file(&path);
        reg.upsert(Entry {
            display_name: d.get("Name").unwrap_or(&name).to_string(),
            binary: d
                .get("Exec")
                .and_then(exec_program)
                .map(PathBuf::from)
                .unwrap_or_else(|| dirs.bin.join(format!("{name}.AppImage"))),
            desktop: desktop_file,
            icon: d.get("Icon").unwrap_or(&name).to_string(),
            source: d
                .get("X-AppImage-Source")
                .map(PathBuf::from)
                .unwrap_or_default(),
            name,
        });
        changed = true;
    }
    changed
}

/// Delete menu entries that duplicate an install of ours.
///
/// Applications commonly register themselves on first run — Electron does it
/// from `app.setAsDefaultProtocolClient()`, writing
/// `~/.local/share/applications/<app>.desktop` with an `Exec` pointing at
/// whatever binary is running, which after our install is *our* copy under
/// `~/.local/bin`. The result is a second, visually identical menu entry.
///
/// An entry whose `Exec` resolves to a binary we installed, and that is not
/// the canonical entry we wrote for it, is by construction a duplicate of
/// ours, so it is safe to drop. Returns the files removed.
fn prune_duplicate_entries(dirs: &Dirs, reg: &Registry) -> Vec<PathBuf> {
    let managed: HashSet<PathBuf> = reg.iter().map(|e| e.binary.clone()).collect();
    let keep: HashSet<&str> = reg.iter().map(|e| e.desktop.as_str()).collect();
    remove_entries_pointing_at(dirs, &managed, &keep)
}

/// Remove every `.desktop` in the applications dir whose `Exec` program is one
/// of `binaries`, except those whose filename is listed in `keep`.
fn remove_entries_pointing_at(
    dirs: &Dirs,
    binaries: &HashSet<PathBuf>,
    keep: &HashSet<&str>,
) -> Vec<PathBuf> {
    let mut removed = Vec::new();
    if binaries.is_empty() {
        return removed;
    }
    let Ok(read_dir) = fs::read_dir(&dirs.applications) else {
        return removed;
    };
    for dir_entry in read_dir.flatten() {
        let path = dir_entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("desktop") {
            continue;
        }
        let file_name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if keep.contains(file_name) {
            continue;
        }
        let Ok(content) = fs::read_to_string(&path) else {
            continue;
        };
        let Some(program) = DesktopEntry::parse(&content)
            .get("Exec")
            .and_then(exec_program)
        else {
            continue;
        };
        if binaries.contains(&PathBuf::from(program)) && fs::remove_file(&path).is_ok() {
            removed.push(path);
        }
    }
    removed
}

/// List installed AppImages, per the registry.
pub fn list() -> io::Result<Vec<InstalledApp>> {
    let dirs = Dirs::ensure()?;
    let (reg, migrated) = open_registry(&dirs)?;
    // Listing is also a convenient moment to clear duplicates an application
    // may have re-created since the last install — which makes `list` double
    // as a repair command.
    let pruned = prune_duplicate_entries(&dirs, &reg);
    if migrated || !pruned.is_empty() {
        refresh_menu(&dirs);
    }
    Ok(reg
        .iter()
        .map(|e| InstalledApp {
            name: e.name.clone(),
            display_name: e.display_name.clone(),
            binary: e.binary.clone(),
            desktop: dirs.applications.join(&e.desktop),
            xdg_warning: None,
        })
        .collect())
}

/// Uninstall by name. Returns true if something was removed.
pub fn uninstall(name: &str) -> Result<bool, InstallError> {
    let dirs = Dirs::ensure()?;
    let (mut reg, _) = open_registry(&dirs)?;
    let Some(entry) = reg.remove(name) else {
        return Ok(false);
    };

    // Remove the binary (only ever from our own bin dir).
    if entry.binary.starts_with(&dirs.bin) && entry.binary.exists() {
        let _ = fs::remove_file(&entry.binary);
    }
    // Remove icons across common sizes.
    if !entry.icon.is_empty() {
        uninstall_icons(&entry.icon);
    }
    // Remove our menu entry, plus any duplicate pointing at the same binary
    // (an app that self-registered would otherwise leave a dead entry behind).
    let _ = fs::remove_file(dirs.applications.join(&entry.desktop));
    remove_entries_pointing_at(&dirs, &HashSet::from([entry.binary]), &HashSet::new());
    reg.save()?;

    let _ = refresh_icon_cache();
    refresh_menu(&dirs);
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
    fn desktop_file_name_is_canonical() {
        // The ID an application would pick for itself, so a self-registering
        // app replaces our entry instead of adding a second one.
        assert_eq!(desktop_file_name("zcode"), "zcode.desktop");
    }

    #[test]
    fn name_from_legacy_desktop_file_roundtrip() {
        assert_eq!(
            name_from_legacy_desktop_file("appimage-manager-zcode.desktop").as_deref(),
            Some("zcode")
        );
    }

    #[test]
    fn name_from_legacy_desktop_file_rejects_foreign() {
        // Files without the 0.1.2 prefix or extension are not migration
        // candidates.
        assert_eq!(name_from_legacy_desktop_file("zcode.desktop"), None);
        assert_eq!(
            name_from_legacy_desktop_file("appimage-manager-zcode"),
            None
        );
        assert_eq!(
            name_from_legacy_desktop_file("appimage-manager-zcode.png"),
            None
        );
    }

    #[test]
    fn exec_program_reads_plain_and_quoted_forms() {
        assert_eq!(
            exec_program("/home/u/.local/bin/zcode.AppImage --no-sandbox %U").as_deref(),
            Some("/home/u/.local/bin/zcode.AppImage")
        );
        // The form Electron writes when it registers itself — the duplicate
        // entry we must be able to recognise.
        assert_eq!(
            exec_program("\"/home/u/.local/bin/zcode.AppImage\" %U").as_deref(),
            Some("/home/u/.local/bin/zcode.AppImage")
        );
        assert_eq!(
            exec_program("\"/home/u/.local/bin/My App.AppImage\" %U").as_deref(),
            Some("/home/u/.local/bin/My App.AppImage")
        );
        assert_eq!(
            exec_program("  /usr/bin/foo").as_deref(),
            Some("/usr/bin/foo")
        );
        assert_eq!(exec_program(""), None);
    }

    #[test]
    fn rewrite_desktop_rewrites_action_execs() {
        let src = DesktopEntry::parse(
            "[Desktop Entry]\n\
             Name=Foo\n\
             Exec=AppRun %U\n\
             Actions=new-window;\n\
             \n\
             [Desktop Action new-window]\n\
             Name=New Window\n\
             Exec=AppRun --new-window\n",
        );
        let out = rewrite_desktop(
            &src,
            Path::new("/home/u/.local/bin/foo.AppImage"),
            "foo",
            Path::new("/home/u/dl/Foo.AppImage"),
            "Foo",
        );
        // The action group survives, with its Exec pointing at the installed
        // binary rather than the unreachable relative AppRun.
        assert_eq!(out.groups.len(), 1);
        assert_eq!(
            out.groups[0].keys,
            vec![
                ("Name".to_string(), "New Window".to_string()),
                (
                    "Exec".to_string(),
                    "/home/u/.local/bin/foo.AppImage --new-window".to_string()
                ),
            ]
        );
    }

    #[test]
    fn rewrite_desktop_guarantees_type_and_clears_hidden() {
        let src = DesktopEntry::parse("[Desktop Entry]\nName=Foo\nExec=AppRun\nHidden=true\n");
        let out = rewrite_desktop(
            &src,
            Path::new("/home/u/.local/bin/foo.AppImage"),
            "foo",
            Path::new("/home/u/dl/Foo.AppImage"),
            "Foo",
        );
        assert_eq!(out.get("Type"), Some("Application"));
        assert_eq!(out.get("Hidden"), None);
    }
}
