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

use crate::installer::Dirs;

/// The desktop file name used for the MIME handler.
pub const HANDLER_DESKTOP: &str = "appimage-handler.desktop";

/// MIME types associated with AppImages that we want to own.
const APPIMAGE_MIME_TYPES: [&str; 3] = [
    "application/vnd.appimage",
    "application/x-appimage",
    "application/octet-stream",
];

/// Outcome of a setup run.
#[derive(Debug)]
pub struct SetupReport {
    pub handler_desktop: PathBuf,
    pub binary: PathBuf,
    /// MIME types we successfully registered as default handler for.
    pub registered: Vec<String>,
    /// MIME types we failed to register (helper missing/error), non-fatal.
    pub failed: Vec<String>,
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
                eprintln!("warn: could not register {mime}: {reason}");
                failed.push(mime.to_string());
            }
        }
    }

    // Refresh desktop database so Dolphin sees the new handler immediately.
    let _ = Command::new("update-desktop-database")
        .arg(&dirs.applications)
        .status();

    Ok(SetupReport {
        handler_desktop: dirs.applications.join(HANDLER_DESKTOP),
        binary,
        registered,
        failed,
    })
}

/// Write the handler `.desktop` file pointing at `binary handle %f`.
fn write_handler_desktop(dirs: &Dirs, binary: &std::path::Path) -> io::Result<()> {
    let path = dirs.applications.join(HANDLER_DESKTOP);
    let content = format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=AppImage Manager\n\
         Comment=Install AppImages with a confirmation prompt\n\
         Exec={bin} handle %f\n\
         Icon=application-x-executable\n\
         NoDisplay=true\n\
         Terminal=false\n\
         MimeType=application/vnd.appimage;application/x-appimage;application/octet-stream;\n\
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
