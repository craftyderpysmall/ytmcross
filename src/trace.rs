//! Opt-in timing log for latency work: `YTMCROSS_TRACE=1` appends
//! millisecond-stamped events to `trace.log` in the data directory.
//! Disabled, `trace!` costs one atomic load.

use std::io::Write;
use std::sync::OnceLock;
use std::time::Instant;

struct Tracer {
    start: Instant,
    file: std::sync::Mutex<std::fs::File>,
}

static TRACER: OnceLock<Option<Tracer>> = OnceLock::new();

fn tracer() -> Option<&'static Tracer> {
    TRACER
        .get_or_init(|| {
            std::env::var_os("YTMCROSS_TRACE")?;
            let path = crate::paths::data_dir().join("trace.log");
            let file = std::fs::OpenOptions::new().create(true).append(true).open(path).ok()?;
            Some(Tracer { start: Instant::now(), file: std::sync::Mutex::new(file) })
        })
        .as_ref()
}

pub fn enabled() -> bool {
    tracer().is_some()
}

/// Private memory of this process in MB (Windows; 0 elsewhere). Trace-only.
pub fn private_mb() -> f64 {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX};
        use windows_sys::Win32::System::Threading::GetCurrentProcess;
        // SAFETY: plain Win32 query into a zeroed, correctly sized struct.
        unsafe {
            let mut c: PROCESS_MEMORY_COUNTERS_EX = std::mem::zeroed();
            c.cb = size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
            GetProcessMemoryInfo(GetCurrentProcess(), (&raw mut c).cast(), c.cb);
            c.PrivateUsage as f64 / 1_048_576.0
        }
    }
    #[cfg(not(windows))]
    {
        0.0
    }
}

pub fn log(msg: std::fmt::Arguments) {
    if let Some(t) = tracer() {
        let ms = t.start.elapsed().as_secs_f64() * 1000.0;
        let thread = std::thread::current();
        let name = thread.name().unwrap_or("?");
        let _ = writeln!(t.file.lock().unwrap(), "{ms:>10.1} ms  [{name}] {msg}");
    }
}

#[macro_export]
macro_rules! trace {
    ($($arg:tt)*) => {
        if $crate::trace::enabled() {
            $crate::trace::log(format_args!($($arg)*));
        }
    };
}
