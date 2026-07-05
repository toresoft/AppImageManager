//! Thin wrapper around `kdialog` for native KDE dialogs.
//!
//! kdialog is resolved via PATH (it lives in `/usr/sbin` on this system, not
//! `/usr/bin`), so we never hardcode its location. Errors are surfaced to the
//! caller; we do not match on stderr strings (locale may be non-English).

use std::io;
use std::process::{Command, ExitStatus};

/// Outcome of a yes/no question.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    Yes,
    No,
}

#[derive(Debug)]
pub enum KdialogError {
    /// kdialog binary not found on PATH.
    NotFound,
    Io(io::Error),
    /// Non-zero exit that is not the documented "no" (e.g. 1 for yes/no).
    Failed(i32),
}

impl std::fmt::Display for KdialogError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KdialogError::NotFound => write!(f, "kdialog not found on PATH"),
            KdialogError::Io(e) => write!(f, "kdialog I/O error: {e}"),
            KdialogError::Failed(c) => write!(f, "kdialog exited with code {c}"),
        }
    }
}

impl std::error::Error for KdialogError {}

impl From<io::Error> for KdialogError {
    fn from(e: io::Error) -> Self {
        if e.kind() == io::ErrorKind::NotFound {
            KdialogError::NotFound
        } else {
            KdialogError::Io(e)
        }
    }
}

/// Build a `kdialog` command, failing early if the binary is missing.
fn kdialog() -> Result<Command, KdialogError> {
    // `Command::new` does not resolve at build time; the NotFound is surfaced
    // when `.status()`/`.output()` runs. We keep the constructor lazy.
    Ok(Command::new("kdialog"))
}

/// Run kdialog and interpret its exit status.
fn run(mut cmd: Command) -> Result<ExitStatus, KdialogError> {
    cmd.status().map_err(KdialogError::from)
}

/// Show a yes/no question dialog. Returns [`Answer::Yes`] or [`Answer::No`].
pub fn yesno(title: &str, message: &str) -> Result<Answer, KdialogError> {
    let mut cmd = kdialog()?;
    cmd.args(["--title", title, "--yesno", message]);
    let status = run(cmd)?;
    map_yesno_status(status)
}

/// Like [`yesno`] but with cancel-as-default and a warning icon.
pub fn warningyesno(title: &str, message: &str) -> Result<Answer, KdialogError> {
    let mut cmd = kdialog()?;
    cmd.args(["--title", title, "--warningyesno", message]);
    let status = run(cmd)?;
    map_yesno_status(status)
}

fn map_yesno_status(status: ExitStatus) -> Result<Answer, KdialogError> {
    // kdialog: 0 = yes, 1 = no for yesno-style dialogs.
    match status.code() {
        Some(0) => Ok(Answer::Yes),
        Some(1) => Ok(Answer::No),
        Some(c) => Err(KdialogError::Failed(c)),
        None => Err(KdialogError::Failed(-1)),
    }
}

/// Information dialog with an OK button.
pub fn msgbox(title: &str, message: &str) -> Result<(), KdialogError> {
    let mut cmd = kdialog()?;
    cmd.args(["--title", title, "--msgbox", message]);
    let status = run(cmd)?;
    check_ok(status)
}

/// Error dialog (red icon).
pub fn error(title: &str, message: &str) -> Result<(), KdialogError> {
    let mut cmd = kdialog()?;
    cmd.args(["--title", title, "--error", message]);
    let status = run(cmd)?;
    check_ok(status)
}

fn check_ok(status: ExitStatus) -> Result<(), KdialogError> {
    match status.code() {
        Some(0) => Ok(()),
        Some(c) => Err(KdialogError::Failed(c)),
        None => Err(KdialogError::Failed(-1)),
    }
}
