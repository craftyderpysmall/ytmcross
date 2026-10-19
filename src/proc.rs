//! Child processes (the solver builder): no console window, and never outliving us.

use std::process::{Child, Command};

/// A command that won't flash a console window and, on Linux, dies with us.
pub fn command(program: &str) -> Command {
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(target_os = "linux")]
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
            Ok(())
        });
    }
    cmd
}

/// Shows `dir` in the system file manager, creating it if needed. Runs the
/// OS's own opener (nothing gets loaded into our process); unlike `command`,
/// the file manager it starts may outlive us. Linux: `file_manager` (a
/// program name) instead of xdg-open when set.
pub fn open_folder(dir: &std::path::Path, file_manager: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let file_manager = file_manager.trim();
    let child = if cfg!(windows) {
        Command::new("explorer").arg(dir).spawn()?
    } else if cfg!(target_os = "macos") {
        Command::new("open").arg(dir).spawn()?
    } else if file_manager.is_empty() {
        Command::new("xdg-open").arg(dir).spawn()?
    } else {
        // Started in the background by a shell that exits at once, so no
        // thread of ours waits for the file manager to be closed.
        Command::new("sh").args(["-c", "\"$0\" \"$1\" >/dev/null 2>&1 &", file_manager]).arg(dir).spawn()?
    };
    // Unix: reap it once it exits (openers return quickly), leaving no zombie.
    #[cfg(unix)]
    {
        let mut child = child;
        let _ = std::thread::Builder::new().name("open-folder".into()).stack_size(64 * 1024).spawn(move || child.wait());
    }
    #[cfg(not(unix))]
    drop(child);
    Ok(())
}

/// Linux file managers, by program name.
const FILE_MANAGERS: &[&str] = &[
    "nautilus",
    "dolphin",
    "thunar",
    "nemo",
    "caja",
    "pcmanfm-qt",
    "pcmanfm",
    "cosmic-files",
    "io.elementary.files",
    "krusader",
    "doublecmd",
    "spacefm",
];

/// The known file managers installed (found on PATH).
pub fn installed_file_managers() -> Vec<&'static str> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let dirs: Vec<_> = std::env::split_paths(&path).collect();
    FILE_MANAGERS.iter().copied().filter(|fm| dirs.iter().any(|d| d.join(fm).is_file())).collect()
}

/// Windows: puts the child (and anything it spawns) in a job object that the
/// OS kills when our process exits, even after a crash.
#[cfg(windows)]
pub fn bind_to_parent(child: &Child) {
    use std::os::windows::io::AsRawHandle;
    use std::sync::OnceLock;
    use windows_sys::Win32::System::JobObjects::*;

    // One job for the whole app; its handle is intentionally never closed:
    // the OS closes it when we exit, which is exactly what kills the children.
    static JOB: OnceLock<usize> = OnceLock::new();
    let job = *JOB.get_or_init(|| unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if !job.is_null() {
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                (&raw const info).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
        }
        job as usize
    });
    if job != 0 {
        unsafe {
            AssignProcessToJobObject(job as _, child.as_raw_handle());
        }
    }
}

#[cfg(not(windows))]
pub fn bind_to_parent(_child: &Child) {}
