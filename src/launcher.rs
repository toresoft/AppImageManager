//! Non-blocking launcher for installed AppImages.
//!
//! After install we start the app in the background. We must not block the
//! `handle` invocation (the file manager waits for our exit) and we must not
//! keep the AppImage mount point tied to our lifetime, so we detach.

use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;

/// Spawn `binary` detached from this process. Errors are best-effort: a
/// launch failure after a successful install is reported but not fatal.
pub fn launch(binary: &Path) -> std::io::Result<()> {
    let mut cmd = Command::new(binary);
    cmd.env_remove("GDK_BACKEND"); // let the app pick its own
    // Detach: new session, redirected stdio, will not be killed with us.
    unsafe {
        cmd.pre_exec(|| {
            // setsid(): become session leader so we survive the parent exit.
            libc_setsid();
            Ok(())
        });
    }
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    cmd.spawn()?;
    Ok(())
}

/// Call `setsid(2)` via raw syscall to avoid pulling a libc crate for a
/// single call. Returns the new session id (ignored here).
unsafe fn libc_setsid() {
    // SYS_setsid on x86_64 is 112; on aarch64 it's 157. We use the libc
    // symbol through the always-available `extern "C"` linkage to `libc`'s
    // `setsid`, but since we have no libc dep we issue the syscall directly.
    // Safer: rely on the `setsid` util in `util-linux` via a shell? No — keep
    // it in-process. We pick the right syscall number per arch.
    #[cfg(target_arch = "x86_64")]
    const SYS_SETSID: i64 = 112;
    #[cfg(target_arch = "aarch64")]
    const SYS_SETSID: i64 = 157;
    #[cfg(target_arch = "x86")]
    const SYS_SETSID: i64 = 112;
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64", target_arch = "x86")))]
    const SYS_SETSID: i64 = -1;

    if SYS_SETSID >= 0 {
        unsafe { syscall1(SYS_SETSID) };
    }
}

unsafe extern "C" {
    fn syscall(num: std::ffi::c_long, ...) -> std::ffi::c_long;
}

unsafe fn syscall1(num: i64) {
    unsafe {
        let _ = syscall(num as std::ffi::c_long);
    }
}
