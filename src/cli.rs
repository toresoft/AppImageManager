//! Command-line interface definition (clap derive).

use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// KDE-native manager for AppImage files.
///
/// Install, list and uninstall AppImages with full menu integration.
/// Designed to be invoked by Dolphin when the user clicks an AppImage.
#[derive(Parser, Debug)]
#[command(
    name = "app-image-manager",
    version,
    about,
    long_about,
    propagate_version = true
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Invoked by the file manager when an AppImage is opened.
    ///
    /// Shows a kdialog confirmation; on accept it installs and launches.
    Handle {
        /// The AppImage file passed by the file manager (%f).
        file: PathBuf,
    },

    /// Install an AppImage without prompting (intended for scripts/CLI use).
    Install {
        /// Path to the AppImage to install.
        file: PathBuf,
    },

    /// List AppImages previously installed by this tool.
    List,

    /// Uninstall an AppImage by its installed name.
    Uninstall {
        /// Name (without extension) as shown by `list`.
        name: String,
        /// Skip the kdialog confirmation prompt (for scripts).
        #[arg(long, short = 'y')]
        yes: bool,
    },

    /// Register this tool as the default handler for AppImage MIME types.
    ///
    /// Idempotent. Run once after installing the binary.
    Setup,
}
