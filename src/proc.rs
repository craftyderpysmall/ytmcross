//! Child processes (yt-dlp): no console window, and never outliving us.

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
