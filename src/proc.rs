//! Child processes, and the C library's heap.

use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};

/// A command the kernel kills if we die, even after a crash. The signal is
/// tied to the spawning thread, so that thread must wait for the child.
pub fn helper(program: impl AsRef<std::ffi::OsStr>) -> Command {
    // SAFETY: no arguments, can't fail.
    let parent = unsafe { libc::getpid() };
    let mut cmd = Command::new(program);
    // SAFETY: only async-signal-safe calls between fork and exec.
    unsafe {
        cmd.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            // We may have died before the line above took effect.
            if libc::getppid() != parent {
                libc::_exit(1);
            }
            Ok(())
        });
    }
    cmd
}

/// Shows `dir` in the file manager, creating it if needed. Nothing gets
/// loaded into our process; unlike `helper`, the window may outlive us.
pub fn open_folder(dir: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let mut child = Command::new("xdg-open").arg(dir).stdout(Stdio::null()).stderr(Stdio::null()).spawn()?;
    // Reaped here, or it would stay a zombie until we exit.
    std::thread::spawn(move || child.wait());
    Ok(())
}

/// One heap for all threads: a thread that exits leaves nothing stranded in
/// an arena of its own, and the pools' threads come and go.
pub fn tune_heap() {
    // SAFETY: called first thing in main, before any other thread exists.
    unsafe { libc::mallopt(libc::M_ARENA_MAX, 1) };
}

/// Hands every free heap page back to the OS, not only those at the top.
pub fn trim_heap() {
    // SAFETY: takes no pointers; glibc locks the heap itself.
    unsafe { libc::malloc_trim(0) };
}
