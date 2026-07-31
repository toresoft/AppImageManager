//! MIME handler registration so Dolphin invokes us when an AppImage is opened.
//!
//! `setup` writes a `appimage-handler.desktop` entry under the user's
//! `~/.local/share/applications` whose `Exec` points at our own absolute binary
//! path with the `handle` subcommand, then registers it as the default app for
//! the relevant AppImage MIME types via `xdg-mime`.

use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::Command;

use rust_i18n::t;

use crate::installer::Dirs;

/// The desktop file name used for the MIME handler.
pub const HANDLER_DESKTOP: &str = "appimage-handler.desktop";

/// MIME types associated with AppImages that we want to own.
///
/// Deliberately limited to the AppImage-specific types. We must NOT claim
/// `application/octet-stream`: that is the generic fallback the freedesktop
/// database assigns to *any* unrecognised binary blob (firmware, `.bin`, disk
/// images, unknown data files). Owning it made the installer prompt pop up on
/// files that have nothing to do with AppImages.
const APPIMAGE_MIME_TYPES: [&str; 2] = ["application/vnd.appimage", "application/x-appimage"];

/// MIME types we registered in earlier versions and now actively disown.
/// `setup` strips these from the user's `mimeapps.list` (only where they point
/// at *our* handler) so upgrading fixes an existing over-broad association.
const OBSOLETE_MIME_TYPES: [&str; 1] = ["application/octet-stream"];

/// Outcome of a setup run.
#[derive(Debug)]
pub struct SetupReport {
    pub handler_desktop: PathBuf,
    pub binary: PathBuf,
    /// MIME types we successfully registered as default handler for.
    pub registered: Vec<String>,
    /// MIME types we failed to register (helper missing/error), non-fatal.
    pub failed: Vec<String>,
    /// Stale over-broad associations removed from the user's `mimeapps.list`.
    pub purged: Vec<String>,
}

/// Locate our own executable path. We prefer `/proc/self/exe` (no symlink
/// issues even if the binary was moved), falling back to `std::env::current_exe`.
pub fn self_exe() -> io::Result<PathBuf> {
    std::env::current_exe()
}

/// Run `setup`: install the handler desktop entry and register MIME defaults.
pub fn setup() -> io::Result<SetupReport> {
    let dirs = Dirs::ensure()?;
    let binary = self_exe()?;
    write_handler_desktop(&dirs, &binary)?;

    let mut registered = Vec::new();
    let mut failed = Vec::new();
    for mime in APPIMAGE_MIME_TYPES {
        match register_default(mime, HANDLER_DESKTOP) {
            Ok(()) => registered.push(mime.to_string()),
            Err(reason) => {
                // Keep going: registering one is better than none.
                eprintln!(
                    "{}",
                    t!("warn_register_mime", mime = mime, reason = reason.as_str())
                );
                failed.push(mime.to_string());
            }
        }
    }

    // Drop associations we should never have claimed (see OBSOLETE_MIME_TYPES).
    let purged = purge_obsolete_associations();

    // Refresh desktop database so Dolphin sees the new handler immediately.
    let _ = Command::new("update-desktop-database")
        .arg(&dirs.applications)
        .status();

    Ok(SetupReport {
        handler_desktop: dirs.applications.join(HANDLER_DESKTOP),
        binary,
        registered,
        failed,
        purged,
    })
}

/// The per-user `mimeapps.list` files XDG clients read, in the order the spec
/// gives them. `xdg-mime` writes to the first; older setups may carry entries
/// in the second.
fn user_mimeapps_files() -> Vec<PathBuf> {
    let mut out = Vec::new();
    let config_home = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")));
    if let Some(c) = config_home {
        out.push(c.join("mimeapps.list"));
    }
    let data_home = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")));
    if let Some(d) = data_home {
        out.push(d.join("applications").join("mimeapps.list"));
    }
    out
}

/// Remove every association that maps an [`OBSOLETE_MIME_TYPES`] entry to our
/// handler, from every per-user `mimeapps.list`.
///
/// Only values equal to [`HANDLER_DESKTOP`] are stripped, so an association
/// the user set to some *other* application survives untouched. Returns the
/// `mime` types actually removed.
fn purge_obsolete_associations() -> Vec<String> {
    let mut purged = Vec::new();
    for path in user_mimeapps_files() {
        let Ok(content) = fs::read_to_string(&path) else {
            continue;
        };
        let (new_content, removed) = strip_obsolete_lines(&content);
        if removed.is_empty() {
            continue;
        }
        if let Err(e) = fs::write(&path, new_content) {
            eprintln!(
                "{}",
                t!(
                    "warn_rewrite_mimeapps",
                    path = path.display().to_string(),
                    err = e
                )
            );
            continue;
        }
        for mime in removed {
            if !purged.contains(&mime) {
                purged.push(mime);
            }
        }
    }
    purged
}

/// Rewrite a `mimeapps.list` body without our obsolete associations.
///
/// Works on any section (`[Default Applications]`, `[Added Associations]`, …)
/// because the key/value shape is the same everywhere: `mime=a.desktop;b.desktop;`.
/// A line that ends up with no desktop files left is dropped entirely.
fn strip_obsolete_lines(content: &str) -> (String, Vec<String>) {
    let mut out = String::with_capacity(content.len());
    let mut removed = Vec::new();

    for line in content.lines() {
        let Some((key, value)) = line.split_once('=') else {
            out.push_str(line);
            out.push('\n');
            continue;
        };
        let mime = key.trim();
        if !OBSOLETE_MIME_TYPES.contains(&mime) {
            out.push_str(line);
            out.push('\n');
            continue;
        }

        // Keep every handler except ours; preserve the trailing `;` convention.
        let kept: Vec<&str> = value
            .split(';')
            .map(str::trim)
            .filter(|v| !v.is_empty() && *v != HANDLER_DESKTOP)
            .collect();
        if kept.len() == value.split(';').filter(|v| !v.trim().is_empty()).count() {
            // Nothing of ours in there: leave the line byte-for-byte alone.
            out.push_str(line);
            out.push('\n');
            continue;
        }

        removed.push(mime.to_string());
        if !kept.is_empty() {
            out.push_str(mime);
            out.push('=');
            out.push_str(&kept.join(";"));
            out.push_str(";\n");
        }
        // else: drop the line entirely.
    }

    (out, removed)
}

/// Presentation fields of the handler entry, translations included.
///
/// The desktop entry spec carries its own per-locale fields (`Name[xx]`,
/// `Comment[xx]`) which the file manager resolves against the user's locale,
/// so these are embedded statically rather than taken from the active runtime
/// locale of *this* process — the entry outlives the process that wrote it.
/// Only `Comment` is translated: the name is a brand, kept identical in every
/// locale (`app_name` in the catalogs), and an untranslated `Name=` already
/// serves as its own English value.
///
/// `packaging/appimage-handler.desktop` ships the same block for the
/// system-wide entry; a test below keeps the two from drifting apart.
const HANDLER_L10N: &str = "\
Name=AppImage Manager
Comment=Install AppImages with a confirmation prompt
Comment[it]=Installa le AppImage con una richiesta di conferma
Comment[es]=Instala AppImages con una solicitud de confirmación
";

/// Write the handler `.desktop` file pointing at `binary handle %f`.
fn write_handler_desktop(dirs: &Dirs, binary: &std::path::Path) -> io::Result<()> {
    let path = dirs.applications.join(HANDLER_DESKTOP);
    let content = format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         {HANDLER_L10N}\
         Exec={bin} handle %f\n\
         Icon=application-x-executable\n\
         NoDisplay=true\n\
         Terminal=false\n\
         MimeType=application/vnd.appimage;application/x-appimage;\n\
         Categories=System;Utility;\n",
        bin = binary.display()
    );
    let mut f = fs::File::create(&path)?;
    f.write_all(content.as_bytes())?;
    Ok(())
}

/// `xdg-mime default <handler> <mime>` — make `handler` the default for `mime`.
fn register_default(mime: &str, handler: &str) -> Result<(), String> {
    let status = Command::new("xdg-mime")
        .args(["default", handler, mime])
        .status()
        .map_err(|e| e.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("xdg-mime exited {}", status))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The user-level entry is written at runtime, the system-level one ships
    /// in the package. Different labels for the same handler show up in KDE's
    /// "Open with" list depending on how the tool was installed.
    #[test]
    fn the_packaged_entry_carries_the_same_labels() {
        let packaged = fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("packaging/appimage-handler.desktop"),
        )
        .expect("packaging/appimage-handler.desktop should be readable");

        for line in HANDLER_L10N.lines() {
            assert!(
                packaged.lines().any(|packaged_line| packaged_line == line),
                "the packaged entry is missing `{line}`"
            );
        }
    }

    #[test]
    fn never_claims_the_generic_binary_type() {
        assert!(
            !APPIMAGE_MIME_TYPES.contains(&"application/octet-stream"),
            "octet-stream is every unrecognised binary; claiming it hijacks unrelated files"
        );
    }

    #[test]
    fn strips_our_obsolete_association() {
        let input = "\
[Default Applications]
application/pdf=okular.desktop
application/octet-stream=appimage-handler.desktop
application/vnd.appimage=appimage-handler.desktop
";
        let (out, removed) = strip_obsolete_lines(input);
        assert_eq!(removed, vec!["application/octet-stream".to_string()]);
        assert!(!out.contains("octet-stream"));
        assert!(out.contains("application/pdf=okular.desktop"));
        assert!(out.contains("application/vnd.appimage=appimage-handler.desktop"));
    }

    #[test]
    fn leaves_other_apps_alone() {
        let input = "\
[Default Applications]
application/octet-stream=ark.desktop
";
        let (out, removed) = strip_obsolete_lines(input);
        assert!(removed.is_empty());
        assert_eq!(out, input);
    }

    #[test]
    fn keeps_the_rest_of_a_multi_value_association() {
        let input = "\
[Added Associations]
application/octet-stream=appimage-handler.desktop;ark.desktop;
";
        let (out, removed) = strip_obsolete_lines(input);
        assert_eq!(removed, vec!["application/octet-stream".to_string()]);
        assert!(out.contains("application/octet-stream=ark.desktop;"));
        assert!(!out.contains("appimage-handler"));
    }
}
