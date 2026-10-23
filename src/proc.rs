//! Child processes (the solver builder): no console window, and never outliving us.

use std::process::{Child, Command};

/// A command that won't flash a console window.
pub fn command(program: &str) -> Command {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let mut cmd = Command::new(program);
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd
}

/// Shows `dir` in Explorer, creating it if needed. Nothing gets loaded into
/// our process; unlike `command`, the window it opens may outlive us.
pub fn open_folder(dir: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    Command::new("explorer").arg(dir).spawn()?;
    Ok(())
}

/// Puts the child (and anything it spawns) in a job object that the OS
/// kills when our process exits, even after a crash.
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
