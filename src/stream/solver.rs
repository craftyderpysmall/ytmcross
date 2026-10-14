//! Unscrambling stream URLs in-process.
//!
//! YouTube scrambles each stream URL's signature and its `n` parameter
//! (without the right `n`, downloads are refused) using functions buried in
//! its ~3 MB player script, which changes every week or so. We run those
//! functions in an embedded QuickJS:
//!
//! 1. Once per player version, in a helper process (`--build-solver`):
//!    download the player and extract the two functions, natively with SWC
//!    (github.com/anenasa/ytdlp-ejs, ~0.3 s) or, if that fails, with yt-dlp's
//!    official solver (github.com/yt-dlp/ejs, ~7 s). The result is saved as
//!    QuickJS bytecode in the data dir; the helper's memory dies with it.
//! 2. Per batch of tracks: load that bytecode into a fresh engine on a
//!    private heap (~25 ms, ~12 MB), unscramble every URL (~2 ms each), drop
//!    the engine and the heap. Nothing stays resident between batches.

use std::ffi::CString;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use rquickjs::{Context, Function, Object, Runtime, qjs};

use crate::{paths, trace};

const EJS_LIB: &str = include_str!("../../assets/ejs/yt.solver.lib.min.js");
const EJS_CORE: &str = include_str!("../../assets/ejs/yt.solver.core.min.js");
/// Bytecode is only valid for the QuickJS build that wrote it.
const BYTECODE_TAG: &str = "qjs014";
/// How long a known player version is trusted before checking again.
const VERSION_TTL: Duration = Duration::from_secs(30 * 60);

#[derive(Clone)]
struct Ready {
    version: String,
    sts: u32,
    bytecode: PathBuf,
}

#[derive(Default)]
struct State {
    ready: Option<Ready>,
    checked_at: Option<Instant>,
}

pub struct SolverStore {
    agent: ureq::Agent,
    state: Mutex<State>,
}

/// A loaded solver. Drop it when the batch is done: all its memory goes back.
pub struct Solver {
    /// Always `Some` until dropped. The context goes before its runtime.
    engine: Option<(Context, Runtime)>,
}

impl Drop for Solver {
    fn drop(&mut self) {
        if let Some((ctx, rt)) = self.engine.take() {
            drop(ctx);
            drop(rt);
            trim_heap();
        }
    }
}

impl SolverStore {
    pub fn new(agent: ureq::Agent) -> SolverStore {
        SolverStore { agent, state: Mutex::default() }
    }

    /// Solver for the current player, if one is ready (never blocks on work).
    pub fn ready(&self) -> Option<(u32, PathBuf)> {
        self.state.lock().unwrap().ready.as_ref().map(|r| (r.sts, r.bytecode.clone()))
    }

    /// Makes sure a solver for the current player version exists, building
    /// one if YouTube shipped a new player. Slow on a new version: call it
    /// from a background thread.
    pub fn refresh(&self, force: bool) -> Result<(), String> {
        {
            let s = self.state.lock().unwrap();
            if !force && s.ready.is_some() && s.checked_at.is_some_and(|t| t.elapsed() < VERSION_TTL) {
                return Ok(());
            }
        }
        let version = player_version(&self.agent)?;
        let (bytecode, sts_file) = files(&version);
        let read_sts = || std::fs::read_to_string(&sts_file).ok().and_then(|s| s.trim().parse::<u32>().ok());
        let sts = match read_sts().filter(|_| bytecode.exists()) {
            Some(sts) => sts,
            None => {
                let t = Instant::now();
                build_in_helper(&version)?;
                trace!("solver: built for player {version} in {:?}", t.elapsed());
                read_sts().filter(|_| bytecode.exists()).ok_or("solver build produced no files")?
            }
        };
        let mut s = self.state.lock().unwrap();
        s.ready = Some(Ready { version, sts, bytecode });
        s.checked_at = Some(Instant::now());
        Ok(())
    }

    /// Forget the current solver (e.g. after YouTube rejected its output).
    pub fn invalidate(&self) {
        let mut s = self.state.lock().unwrap();
        if let Some(r) = s.ready.take() {
            trace!("solver: invalidated player {}", r.version);
        }
        s.checked_at = None;
    }

}

fn files(version: &str) -> (PathBuf, PathBuf) {
    let dir = paths::data_dir().join("solver");
    (dir.join(format!("{version}-{BYTECODE_TAG}.bc")), dir.join(format!("{version}.sts")))
}

fn player_version(agent: &ureq::Agent) -> Result<String, String> {
    let t = Instant::now();
    let text = agent
        .get("https://www.youtube.com/iframe_api")
        .call()
        .and_then(|mut r| r.body_mut().read_to_string())
        .map_err(|e| format!("player version: {e}"))?;
    let version = text
        .split("player\\/")
        .nth(1)
        .and_then(|rest| rest.split("\\/").next())
        .filter(|v| !v.is_empty() && v.len() < 32 && v.bytes().all(|b| b.is_ascii_alphanumeric()))
        .ok_or("player version not found")?
        .to_string();
    trace!("solver: player version {version} ({:?})", t.elapsed());
    Ok(version)
}

/// Builds in a short-lived helper process (`ytmcross --build-solver`):
/// parsing a 3 MB script briefly needs ~100 MB, and all of it goes back to
/// the OS the moment the helper exits. A crash there can't take the app down.
fn build_in_helper(version: &str) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut child = crate::proc::command(&exe.to_string_lossy())
        .args(["--build-solver", version])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("solver helper: {e}"))?;
    crate::proc::bind_to_parent(&child);
    match child.wait() {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(format!("solver helper failed ({status})")),
        Err(e) => Err(e.to_string()),
    }
}

/// Entry point of `ytmcross --build-solver <version>`. Exit code 0 on success.
pub fn build_cli(version: &str) -> i32 {
    match build(version) {
        Ok(()) => 0,
        Err(e) => {
            trace!("solver helper: {e}");
            1
        }
    }
}

fn build(version: &str) -> Result<(), String> {
    let agent = crate::api::client::agent();
    let url = format!("https://www.youtube.com/s/player/{version}/player_ias.vflset/en_US/base.js");
    let t = Instant::now();
    let player = agent
        .get(&url)
        .call()
        .and_then(|mut r| r.body_mut().with_config().limit(32 << 20).read_to_string())
        .map_err(|e| format!("download player: {e}"))?;
    trace!("solver helper: downloaded player in {:?}", t.elapsed());
    let sts = signature_timestamp(&player).ok_or("signature timestamp not found")?;

    // Native SWC preprocessing (~0.3 s) first; yt-dlp's official solver
    // (~7 s, more memory) when SWC doesn't understand a new player.
    let mut last_error = String::new();
    let mut code = None;
    for (name, preprocess) in [("swc", preprocess_swc as fn(&str) -> Result<String, String>), ("official", preprocess_official)] {
        let t = Instant::now();
        let attempt = preprocess(&player).and_then(|pre| compile(&pre)).and_then(|bc| {
            // Validate before saving: `n` (needed by every stream) must run.
            // A broken `sig` shows up as a refused URL and falls back to yt-dlp.
            let solver = Solver::from_bytecode(&bc)?;
            solver.n("ytmcrossprobe123")?;
            Ok(bc)
        });
        match attempt {
            Ok(bc) => {
                trace!("solver helper: {name} preprocessing ok in {:?}", t.elapsed());
                code = Some(bc);
                break;
            }
            Err(e) => {
                trace!("solver helper: {name} preprocessing failed: {e}");
                last_error = e;
            }
        }
    }
    let code = code.ok_or(last_error)?;

    let (bytecode, sts_file) = files(version);
    let dir = bytecode.parent().expect("solver dir");
    let _ = std::fs::create_dir_all(dir);
    // Old players' files are useless now.
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let _ = std::fs::remove_file(e.path());
        }
    }
    std::fs::write(&bytecode, &code).map_err(|e| e.to_string())?;
    // The sts file is written last: its presence marks a complete build.
    std::fs::write(&sts_file, sts.to_string()).map_err(|e| e.to_string())
}

fn preprocess_swc(player: &str) -> Result<String, String> {
    ytdlp_ejs::preprocess_player(player).map_err(|e| e.to_string())
}

impl Solver {
    pub fn load(bytecode: &PathBuf) -> Result<Solver, String> {
        let t = Instant::now();
        let bytes = std::fs::read(bytecode).map_err(|e| e.to_string())?;
        let solver = Solver::from_bytecode(&bytes)?;
        trace!("solver: loaded in {:?}", t.elapsed());
        Ok(solver)
    }

    fn from_bytecode(bytes: &[u8]) -> Result<Solver, String> {
        let rt = runtime()?;
        rt.set_max_stack_size(32 << 20);
        let ctx = Context::full(&rt).map_err(|e| e.to_string())?;
        ctx.with(|ctx| -> Result<(), String> {
            let result = Object::new(ctx.clone()).map_err(|e| e.to_string())?;
            ctx.globals().set("_result", result).map_err(|e| e.to_string())?;
            let raw = ctx.as_raw().as_ptr();
            // SAFETY: plain QuickJS C API calls on a live context; every value
            // we create is freed before returning.
            unsafe {
                let fun = qjs::JS_ReadObject(raw, bytes.as_ptr(), bytes.len() as _, qjs::JS_READ_OBJ_BYTECODE as i32);
                if qjs::JS_IsException(fun) {
                    let _ = ctx.catch();
                    return Err("invalid solver bytecode".into());
                }
                let res = qjs::JS_EvalFunction(raw, fun);
                let failed = qjs::JS_IsException(res);
                qjs::JS_FreeValue(raw, res);
                if failed {
                    return Err(format!("solver failed to start: {:?}", ctx.catch()));
                }
            }
            Ok(())
        })?;
        Ok(Solver { engine: Some((ctx, rt)) })
    }

    fn call(&self, name: &str, input: &str) -> Result<String, String> {
        let (ctx, _) = self.engine.as_ref().ok_or("solver dropped")?;
        ctx.with(|ctx| {
            let result: Object = ctx.globals().get("_result").map_err(|e| e.to_string())?;
            let f: Function = result.get(name).map_err(|_| format!("no {name} function"))?;
            f.call((input,)).map_err(|e| match e {
                rquickjs::Error::Exception => format!("{name}: {:?}", ctx.catch()),
                e => format!("{name}: {e}"),
            })
        })
    }

    pub fn n(&self, challenge: &str) -> Result<String, String> {
        self.call("n", challenge)
    }

    pub fn sig(&self, challenge: &str) -> Result<String, String> {
        self.call("sig", challenge)
    }
}

/// A QuickJS runtime whose memory is fully returned to the OS when dropped.
///
/// Windows: every solver runtime gets its own private heap, destroyed in one
/// go afterwards; otherwise its ~12 MB would leave the process heap
/// fragmented and the app's footprint would creep up with every batch.
/// Elsewhere: the default allocator, then a trim where glibc supports it.
fn runtime() -> Result<Runtime, String> {
    #[cfg(windows)]
    let rt = match private_heap::PrivateHeap::new() {
        Some(heap) => Runtime::new_with_alloc(heap),
        None => Runtime::new(),
    };
    #[cfg(not(windows))]
    let rt = Runtime::new();
    rt.map_err(|e| e.to_string())
}

/// After dropping a runtime: give freed memory back to the OS where needed.
fn trim_heap() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    // SAFETY: malloc_trim only releases free memory.
    unsafe {
        libc::malloc_trim(0);
    }
}

#[cfg(windows)]
mod private_heap {
    use rquickjs::allocator::Allocator;
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::System::Memory::{HEAP_ZERO_MEMORY, HeapAlloc, HeapCreate, HeapDestroy, HeapFree, HeapReAlloc};

    /// Each block starts with its size, so `usable_size` (which gets no
    /// heap handle) can answer. 16 bytes keeps the 16-byte alignment.
    const HEADER: usize = 16;

    pub struct PrivateHeap(HANDLE);

    // SAFETY: the heap is created serialized (thread-safe) and only used by
    // the runtime that owns it.
    unsafe impl Send for PrivateHeap {}

    impl PrivateHeap {
        pub fn new() -> Option<PrivateHeap> {
            // SAFETY: plain Win32 call; a null handle means failure.
            let heap = unsafe { HeapCreate(0, 0, 0) };
            (!heap.is_null()).then_some(PrivateHeap(heap))
        }

        /// SAFETY: `block` must be null or come from this heap's HeapAlloc/HeapReAlloc.
        unsafe fn finish(block: *mut u8, size: usize) -> *mut u8 {
            if block.is_null() {
                return block;
            }
            unsafe {
                block.cast::<usize>().write(size);
                block.add(HEADER)
            }
        }
    }

    impl Drop for PrivateHeap {
        fn drop(&mut self) {
            // SAFETY: rquickjs drops the allocator after JS_FreeRuntime, so
            // nothing references this heap any more.
            unsafe {
                HeapDestroy(self.0);
            }
        }
    }

    // SAFETY: blocks are valid for the requested size, 16-byte aligned, and
    // freed/resized only through the same heap.
    unsafe impl Allocator for PrivateHeap {
        fn alloc(&mut self, size: usize) -> *mut u8 {
            unsafe { Self::finish(HeapAlloc(self.0, 0, size + HEADER).cast(), size) }
        }

        fn calloc(&mut self, count: usize, size: usize) -> *mut u8 {
            let Some(total) = count.checked_mul(size) else { return std::ptr::null_mut() };
            unsafe { Self::finish(HeapAlloc(self.0, HEAP_ZERO_MEMORY, total + HEADER).cast(), total) }
        }

        unsafe fn dealloc(&mut self, ptr: *mut u8) {
            if !ptr.is_null() {
                unsafe {
                    HeapFree(self.0, 0, ptr.sub(HEADER).cast());
                }
            }
        }

        unsafe fn realloc(&mut self, ptr: *mut u8, new_size: usize) -> *mut u8 {
            if ptr.is_null() {
                return self.alloc(new_size);
            }
            if new_size == 0 {
                unsafe { self.dealloc(ptr) };
                return std::ptr::null_mut();
            }
            unsafe { Self::finish(HeapReAlloc(self.0, 0, ptr.sub(HEADER).cast(), new_size + HEADER).cast(), new_size) }
        }

        unsafe fn usable_size(ptr: *mut u8) -> usize {
            unsafe { ptr.sub(HEADER).cast::<usize>().read() }
        }
    }
}

/// `signatureTimestamp:20737` (or `sts:20737` in some players).
fn signature_timestamp(player: &str) -> Option<u32> {
    for key in ["signatureTimestamp:", "sts:"] {
        let mut rest = player;
        while let Some(i) = rest.find(key) {
            rest = &rest[i + key.len()..];
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if digits.len() == 5 {
                return digits.parse().ok();
            }
        }
    }
    None
}

/// Runs yt-dlp's official solver's preprocessing step on the player script.
fn preprocess_official(player: &str) -> Result<String, String> {
    let rt = runtime()?;
    rt.set_max_stack_size(64 << 20);
    let ctx = Context::full(&rt).map_err(|e| e.to_string())?;
    ctx.with(|ctx| {
        let fail = |e: rquickjs::Error| match e {
            rquickjs::Error::Exception => format!("solver: {:?}", ctx.catch()),
            e => format!("solver: {e}"),
        };
        ctx.eval::<(), _>(EJS_LIB).map_err(fail)?;
        ctx.eval::<(), _>("globalThis.meriyah = lib.meriyah; globalThis.astring = lib.astring;").map_err(fail)?;
        ctx.eval::<(), _>(EJS_CORE).map_err(fail)?;
        ctx.globals().set("__player", player).map_err(fail)?;
        let out: Object = ctx
            .eval("jsc({type: 'player', player: __player, requests: [], output_preprocessed: true})")
            .map_err(fail)?;
        out.get::<_, String>("preprocessed_player").map_err(|_| "solver produced no output".to_string())
    })
}

/// Compiles the preprocessed player to stripped QuickJS bytecode.
fn compile(code: &str) -> Result<Vec<u8>, String> {
    let rt = runtime()?;
    let ctx = Context::full(&rt).map_err(|e| e.to_string())?;
    let src = CString::new(code).map_err(|e| e.to_string())?;
    let name = CString::new("player").unwrap();
    ctx.with(|ctx| {
        let raw = ctx.as_raw().as_ptr();
        // SAFETY: as above; the serialized buffer is copied, then freed.
        unsafe {
            let fun = qjs::JS_Eval(
                raw,
                src.as_ptr(),
                code.len() as _,
                name.as_ptr(),
                (qjs::JS_EVAL_TYPE_GLOBAL | qjs::JS_EVAL_FLAG_COMPILE_ONLY) as i32,
            );
            if qjs::JS_IsException(fun) {
                return Err(format!("compile: {:?}", ctx.catch()));
            }
            let mut size: qjs::size_t = 0;
            let flags = qjs::JS_WRITE_OBJ_BYTECODE | qjs::JS_WRITE_OBJ_STRIP_SOURCE | qjs::JS_WRITE_OBJ_STRIP_DEBUG;
            let buf = qjs::JS_WriteObject(raw, &mut size, fun, flags as i32);
            qjs::JS_FreeValue(raw, fun);
            if buf.is_null() {
                return Err("serialize failed".into());
            }
            let out = std::slice::from_raw_parts(buf, size as usize).to_vec();
            qjs::js_free(raw, buf.cast());
            Ok(out)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_signature_timestamp() {
        assert_eq!(signature_timestamp("a,signatureTimestamp:20737,b"), Some(20737));
        assert_eq!(signature_timestamp("x=1;sts:123;y={sts:20500}"), Some(20500));
        assert_eq!(signature_timestamp("nothing here"), None);
    }
}
