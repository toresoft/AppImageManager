//! AppImage Manager — KDE-native installer for AppImages.
//!
//! See the README for the full picture. In short: register with `setup`,
//! then clicking an AppImage in Dolphin asks for confirmation, installs it
//! under `~/.local/bin` with a KDE menu entry, and launches it.

mod appimage;
mod cli;
mod desktop;
mod installer;
mod kdialog;
mod launcher;
mod metadata;
mod mime;

use std::path::Path;
use std::process::ExitCode;

use clap::Parser;

use cli::{Cli, Command};
use installer::uninstall;

fn main() -> ExitCode {
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
    // Sanity: file must exist and look like an AppImage before prompting.
    if !file.exists() {
        show_error("AppImage Manager", "Il file non esiste.");
        return ExitCode::FAILURE;
    }
    if !is_appimage(file) {
        show_error(
            "AppImage Manager",
            "Il file non sembra essere una AppImage valida.",
        );
        return ExitCode::FAILURE;
    }

    // Resolve a display name early so the prompt is meaningful.
    let display = display_name_guess(file);
    let prompt = format!(
        "Vuoi installare «{display}»?\n\n\
         L'AppImage verrà copiata in ~/.local/bin e verrà creata la voce nel menù di KDE."
    );
    match kdialog::yesno("Installa AppImage", &prompt) {
        Ok(kdialog::Answer::Yes) => {}
        Ok(kdialog::Answer::No) => return ExitCode::SUCCESS,
        Err(e) => {
            // Without a way to ask, fall back to stderr + failure.
            eprintln!("kdialog error: {e}");
            return ExitCode::FAILURE;
        }
    }

    match installer::install(file) {
        Ok(installed) => {
            // Compose the success message, appending the XDG hint when the
            // environment would hide the menu entry.
            let mut msg = format!("«{}» installata con successo.", installed.display_name);
            if let Some(warn) = &installed.xdg_warning {
                msg.push_str("\n\n");
                msg.push_str(warn);
            }
            let _ = kdialog::msgbox("AppImage Manager", &msg);
            // Launch in the background (best-effort).
            if let Err(e) = launcher::launch(&installed.binary) {
                eprintln!("warn: avvio fallito: {e}");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            let msg = format!("Installazione non riuscita:\n{e}");
            let _ = kdialog::error("AppImage Manager", &msg);
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
                "Installata: {} ({})",
                installed.display_name,
                installed.binary.display()
            );
            if let Some(warn) = &installed.xdg_warning {
                eprintln!("\nAVVISO: {warn}");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("errore: {e}");
            ExitCode::FAILURE
        }
    }
}

fn list_cmd() -> ExitCode {
    match installer::list() {
        Ok(items) if items.is_empty() => {
            println!("Nessuna AppImage installata.");
            ExitCode::SUCCESS
        }
        Ok(items) => {
            // Align columns for readability.
            let name_w = items.iter().map(|i| i.name.len()).max().unwrap_or(4);
            let disp_w = items
                .iter()
                .map(|i| i.display_name.len())
                .max()
                .unwrap_or(8);
            println!(
                "{:<width_n$}  {:<width_d$}  BINARIO",
                "NOME",
                "NOME VISUALIZZATO",
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
            eprintln!("errore: {e}");
            ExitCode::FAILURE
        }
    }
}

fn uninstall_cmd(name: &str, yes: bool) -> ExitCode {
    // Confirm via kdialog unless `--yes` was passed.
    if !yes {
        let prompt = format!("Rimuovere «{name}» e la sua voce di menù?");
        match kdialog::warningyesno("AppImage Manager", &prompt) {
            Ok(kdialog::Answer::No) => return ExitCode::SUCCESS,
            Ok(kdialog::Answer::Yes) => {}
            Err(_) => {
                // Non-interactive fallback: proceed without prompt.
            }
        }
    }

    match uninstall(name) {
        Ok(true) => {
            let _ = kdialog::msgbox("AppImage Manager", &format!("«{name}» rimossa."));
            println!("rimossa: {name}");
            ExitCode::SUCCESS
        }
        Ok(false) => {
            let msg = format!("Nessuna AppImage installata con nome «{name}».");
            let _ = kdialog::error("AppImage Manager", &msg);
            eprintln!("{msg}");
            ExitCode::FAILURE
        }
        Err(e) => {
            let msg = format!("Disinstallazione non riuscita:\n{e}");
            let _ = kdialog::error("AppImage Manager", &msg);
            eprintln!("{msg}");
            ExitCode::FAILURE
        }
    }
}

fn setup_cmd() -> ExitCode {
    match mime::setup() {
        Ok(report) => {
            println!("Handler registrato: {}", report.handler_desktop.display());
            println!("Binario: {}", report.binary.display());
            if !report.registered.is_empty() {
                println!(
                    "MIME registrati come default: {}",
                    report.registered.join(", ")
                );
            }
            if !report.failed.is_empty() {
                println!(
                    "MIME non registrati (verifica xdg-mime): {}",
                    report.failed.join(", ")
                );
            }
            if !report.purged.is_empty() {
                println!(
                    "Associazioni troppo generiche rimosse: {}",
                    report.purged.join(", ")
                );
            }
            println!(
                "\nOra il click su un'AppImage in Dolphin aprirà la conferma di installazione."
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("setup non riuscito: {e}");
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
