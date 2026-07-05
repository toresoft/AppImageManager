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
    if !is_appimage_extension(file) && !looks_like_appimage(file) {
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
            let _ = kdialog::msgbox(
                "AppImage Manager",
                &format!("«{}» installata con successo.", installed.display_name),
            );
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

/// Cheap extension check.
fn is_appimage_extension(p: &Path) -> bool {
    let Some(name) = p.file_name().and_then(|s| s.to_str()) else {
        return false;
    };
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".appimage")
}

/// Cheap magic-byte check (ELF + AI\x02) without the full squashfs scan.
fn looks_like_appimage(p: &Path) -> bool {
    use std::io::Read;
    let Ok(mut f) = std::fs::File::open(p) else {
        return false;
    };
    let mut buf = [0u8; 11];
    if f.read_exact(&mut buf).is_err() {
        return false;
    }
    &buf[0..4] == b"\x7fELF" && &buf[8..10] == b"AI" && buf[10] == 0x02
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
