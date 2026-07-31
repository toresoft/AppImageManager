//! AppImage Manager — KDE-native installer for AppImages.
//!
//! See the README for the full picture. In short: register with `setup`,
//! then clicking an AppImage in Dolphin asks for confirmation, installs it
//! under `~/.local/bin` with a KDE menu entry, and launches it.

// Load the translation catalogs (locales/*.yml) into the binary and expose
// `t!`. Must live at crate root so the `t!` macro resolves `_rust_i18n_t`
// here. This only sets `en` as the fallback for missing keys — the active
// locale stays `en` until `i18n::init` reads the environment, since rust-i18n
// itself never consults `LANG`/`LC_*`. See build.rs for the rebuild trigger.
rust_i18n::i18n!("locales", fallback = "en");

mod appimage;
mod cli;
mod desktop;
mod i18n;
mod installer;
mod kdialog;
mod launcher;
mod metadata;
mod mime;
mod registry;

use std::path::Path;
use std::process::ExitCode;

use clap::Parser;
use rust_i18n::t;

use cli::{Cli, Command};
use installer::uninstall;

fn main() -> ExitCode {
    // Before anything user-facing: pick the language from the session.
    i18n::init();

    let cli = Cli::parse();
    match cli.command {
        Command::Handle { file } => handle(&file),
        Command::Install { file } => install_silent(&file),
        Command::List => list_cmd(),
        Command::Uninstall { name, yes } => uninstall_cmd(&name, yes),
        Command::Setup => setup_cmd(),
    }
}

/// `handle <file>` — invoked by the file manager.
///
/// Asks for confirmation via kdialog, installs, then launches.
fn handle(file: &Path) -> ExitCode {
    let app_name = t!("app_name").to_string();
    let install_app = t!("install_app").to_string();

    // Sanity: file must exist and look like an AppImage before prompting.
    if !file.exists() {
        show_error(&app_name, t!("err_file_not_exist").as_ref());
        return ExitCode::FAILURE;
    }
    if !is_appimage(file) {
        show_error(&app_name, t!("err_not_appimage").as_ref());
        return ExitCode::FAILURE;
    }

    // Resolve a display name early so the prompt is meaningful.
    let display = display_name_guess(file);
    let prompt = t!("prompt_install", name = display).to_string();
    match kdialog::yesno(&install_app, &prompt) {
        Ok(kdialog::Answer::Yes) => {}
        Ok(kdialog::Answer::No) => return ExitCode::SUCCESS,
        Err(e) => {
            // Without a way to ask, fall back to stderr + failure.
            eprintln!("{}", t!("err_kdialog", err = e));
            return ExitCode::FAILURE;
        }
    }

    match installer::install(file) {
        Ok(installed) => {
            // Compose the success message, appending the XDG hint when the
            // environment would hide the menu entry.
            let mut msg = t!("msg_installed_ok", name = installed.display_name).to_string();
            if let Some(warn) = &installed.xdg_warning {
                msg.push_str("\n\n");
                msg.push_str(warn);
            }
            let _ = kdialog::msgbox(&app_name, &msg);
            // Launch in the background (best-effort).
            if let Err(e) = launcher::launch(&installed.binary) {
                eprintln!("{}", t!("warn_launch_failed", err = e));
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            let msg = t!("err_install_failed", err = e).to_string();
            let _ = kdialog::error(&app_name, &msg);
            eprintln!("{msg}");
            ExitCode::FAILURE
        }
    }
}

/// `install <file>` — non-interactive install (CLI/scripts).
fn install_silent(file: &Path) -> ExitCode {
    match installer::install(file) {
        Ok(installed) => {
            println!(
                "{}",
                t!(
                    "msg_installed_cli",
                    name = installed.display_name,
                    bin = installed.binary.display().to_string()
                )
            );
            if let Some(warn) = &installed.xdg_warning {
                // Blank line first: the warning is a wall of text following a
                // one-line success message.
                eprintln!("\n{}", t!("notice_prefix", warn = warn.as_str()));
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{}", t!("err_prefix", err = e));
            ExitCode::FAILURE
        }
    }
}

fn list_cmd() -> ExitCode {
    match installer::list() {
        Ok(items) if items.is_empty() => {
            println!("{}", t!("msg_no_installs"));
            ExitCode::SUCCESS
        }
        Ok(items) => {
            // Align columns for readability. Headers carry display-markup
            // glyphs that may be multi-byte; column width is measured in
            // chars, which is what the format pad spec counts too.
            let hdr_name = t!("hdr_name").to_string();
            let hdr_display = t!("hdr_display").to_string();
            let hdr_binary = t!("hdr_binary").to_string();
            let name_w = items
                .iter()
                .map(|i| i.name.chars().count())
                .max()
                .unwrap_or(0)
                .max(hdr_name.chars().count());
            let disp_w = items
                .iter()
                .map(|i| i.display_name.chars().count())
                .max()
                .unwrap_or(0)
                .max(hdr_display.chars().count());
            println!(
                "{:<width_n$}  {:<width_d$}  {}",
                hdr_name,
                hdr_display,
                hdr_binary,
                width_n = name_w,
                width_d = disp_w
            );
            for it in items {
                println!(
                    "{:<name_w$}  {:<disp_w$}  {}",
                    it.name,
                    it.display_name,
                    it.binary.display(),
                    name_w = name_w,
                    disp_w = disp_w,
                );
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{}", t!("err_prefix", err = e));
            ExitCode::FAILURE
        }
    }
}

fn uninstall_cmd(name: &str, yes: bool) -> ExitCode {
    let app_name = t!("app_name").to_string();
    // Confirm via kdialog unless `--yes` was passed.
    if !yes {
        let prompt = t!("prompt_uninstall", name = name).to_string();
        match kdialog::warningyesno(&app_name, &prompt) {
            Ok(kdialog::Answer::No) => return ExitCode::SUCCESS,
            Ok(kdialog::Answer::Yes) => {}
            Err(_) => {
                // Non-interactive fallback: proceed without prompt.
            }
        }
    }

    match uninstall(name) {
        Ok(true) => {
            let _ = kdialog::msgbox(&app_name, t!("msg_removed", name = name).as_ref());
            println!("{}", t!("msg_removed_cli", name = name));
            ExitCode::SUCCESS
        }
        Ok(false) => {
            let msg = t!("err_not_found", name = name).to_string();
            let _ = kdialog::error(&app_name, &msg);
            eprintln!("{msg}");
            ExitCode::FAILURE
        }
        Err(e) => {
            let msg = t!("err_uninstall_failed", err = e).to_string();
            let _ = kdialog::error(&app_name, &msg);
            eprintln!("{msg}");
            ExitCode::FAILURE
        }
    }
}

fn setup_cmd() -> ExitCode {
    match mime::setup() {
        Ok(report) => {
            println!(
                "{}",
                t!(
                    "setup_handler",
                    path = report.handler_desktop.display().to_string()
                )
            );
            println!(
                "{}",
                t!("setup_binary", path = report.binary.display().to_string())
            );
            if !report.registered.is_empty() {
                println!(
                    "{}",
                    t!("setup_mime_registered", list = report.registered.join(", "))
                );
            }
            if !report.failed.is_empty() {
                println!(
                    "{}",
                    t!("setup_mime_failed", list = report.failed.join(", "))
                );
            }
            if !report.purged.is_empty() {
                println!("{}", t!("setup_purged", list = report.purged.join(", ")));
            }
            println!("{}", t!("setup_done"));
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{}", t!("err_setup_failed", err = e));
            ExitCode::FAILURE
        }
    }
}

/// Does this file actually look like an AppImage?
///
/// Content decides, never the name alone. The authoritative signal is the
/// type-2 AppImage magic (`\x7fELF` … `AI\x02`). A few builds ship with the
/// magic zeroed out, so we also accept an ELF binary *named* `*.AppImage` —
/// but never a non-ELF file, whatever it is called.
fn is_appimage(p: &Path) -> bool {
    match read_header(p) {
        Some(h) if is_appimage_magic(&h) => true,
        Some(h) => is_elf(&h) && is_appimage_extension(p),
        None => false,
    }
}

/// First 11 bytes of the file, or `None` if it is unreadable or shorter.
fn read_header(p: &Path) -> Option<[u8; 11]> {
    use std::io::Read;
    let mut f = std::fs::File::open(p).ok()?;
    let mut buf = [0u8; 11];
    f.read_exact(&mut buf).ok()?;
    Some(buf)
}

fn is_elf(header: &[u8; 11]) -> bool {
    &header[0..4] == b"\x7fELF"
}

/// Magic-byte check (ELF + `AI\x02`) without the full squashfs scan.
fn is_appimage_magic(header: &[u8; 11]) -> bool {
    is_elf(header) && &header[8..10] == b"AI" && header[10] == 0x02
}

/// Cheap extension check.
fn is_appimage_extension(p: &Path) -> bool {
    let Some(name) = p.file_name().and_then(|s| s.to_str()) else {
        return false;
    };
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".appimage")
}

/// Best-effort display name for the confirmation prompt, derived from the
/// filename stem before we parse the desktop entry.
fn display_name_guess(p: &Path) -> String {
    p.file_stem()
        .and_then(|s| s.to_str())
        .map(str::to_string)
        .unwrap_or_else(|| "AppImage".to_string())
}

fn show_error(title: &str, msg: &str) {
    let _ = kdialog::error(title, msg);
    eprintln!("{msg}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Write `bytes` to a uniquely named temp file and return its path.
    fn temp_file(name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("aim-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(bytes).unwrap();
        path
    }

    fn appimage_bytes() -> Vec<u8> {
        let mut v = b"\x7fELF\x02\x01\x01\x00AI\x02".to_vec();
        v.extend_from_slice(&[0u8; 32]);
        v
    }

    #[test]
    fn accepts_a_real_appimage() {
        let p = temp_file("real.AppImage", &appimage_bytes());
        assert!(is_appimage(&p));
    }

    #[test]
    fn accepts_an_appimage_without_extension() {
        let p = temp_file("no-extension", &appimage_bytes());
        assert!(is_appimage(&p), "the magic alone must be enough");
    }

    #[test]
    fn rejects_an_arbitrary_binary_blob() {
        // The exact case the octet-stream association used to trigger on.
        let p = temp_file(
            "firmware.bin",
            &[0xde, 0xad, 0xbe, 0xef, 0, 1, 2, 3, 4, 5, 6, 7],
        );
        assert!(!is_appimage(&p));
    }

    #[test]
    fn rejects_a_non_elf_file_named_appimage() {
        let p = temp_file("liar.AppImage", b"PK\x03\x04 not an appimage at all");
        assert!(!is_appimage(&p), "the extension alone must not be enough");
    }

    #[test]
    fn accepts_an_elf_named_appimage_without_magic() {
        // Some builds zero the AppImage magic; the extension carries the intent.
        let mut bytes = b"\x7fELF\x02\x01\x01\x00\x00\x00\x00".to_vec();
        bytes.extend_from_slice(&[0u8; 32]);
        let p = temp_file("stripped-magic.AppImage", &bytes);
        assert!(is_appimage(&p));
    }

    #[test]
    fn rejects_a_file_too_short_to_have_a_header() {
        let p = temp_file("tiny.AppImage", b"\x7fELF");
        assert!(!is_appimage(&p));
    }
}
