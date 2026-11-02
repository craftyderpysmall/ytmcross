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
//!    QuickJS bytecode in the cache dir; the helper's memory dies with it.
//! 2. Per batch of tracks: load that bytecode into a fresh engine on a
//!    runtime (~25 ms, ~12 MB), unscramble every URL (~2 ms each), drop the
//!    engine and trim the heap. Nothing stays resident between batches.

use std::ffi::CString;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use rquickjs::{Context, Function, Object, Runtime, qjs};

use crate::{paths, trace};

// yt-dlp's solver scripts, deflated by build.rs (see `inflate`).
const EJS_LIB: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/yt.solver.lib.min.js.deflate"));
const EJS_CORE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/yt.solver.core.min.js.deflate"));
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
    /// Held while checking the version and building: downloads refresh too,
    /// and two threads must not build the same solver at once.
    refreshing: Mutex<()>,
}

/// A loaded solver. Drop it when the batch is done: all its memory goes back.
pub struct Solver {
    /// Dropped in order: the context before its runtime.
    engine: (Context, Runtime),
}

impl SolverStore {
    pub fn new(agent: ureq::Agent) -> SolverStore {
        SolverStore { agent, state: Mutex::default(), refreshing: Mutex::new(()) }
    }

    /// Solver for the current player, if one is ready (never blocks on work).
    pub fn ready(&self) -> Option<(u32, PathBuf)> {
        self.state.lock().unwrap().ready.as_ref().map(|r| (r.sts, r.bytecode.clone()))
    }

    /// Makes sure a solver for the current player version exists, building
    /// one if YouTube shipped a new player. Slow on a new version: call it
    /// from a background thread.
    pub fn refresh(&self, force: bool) -> Result<(), String> {
        let fresh = || {
            let s = self.state.lock().unwrap();
            s.ready.is_some() && s.checked_at.is_some_and(|t| t.elapsed() < VERSION_TTL)
        };
        if !force && fresh() {
            return Ok(());
        }
        let _one_at_a_time = self.refreshing.lock().unwrap();
        // Another thread may have just done it.
        if !force && fresh() {
            return Ok(());
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
    let dir = paths::cache_dir().join("solver");
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
    let mut child = crate::proc::helper(exe)
        .args(["--build-solver", version])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("solver helper: {e}"))?;
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
            // A broken `sig` shows up as a refused URL, which rebuilds the solver.
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
        let rt = Runtime::new().map_err(|e| e.to_string())?;
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
        Ok(Solver { engine: (ctx, rt) })
    }

    fn call(&self, name: &str, input: &str) -> Result<String, String> {
        let (ctx, _) = &self.engine;
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
    let rt = Runtime::new().map_err(|e| e.to_string())?;
    rt.set_max_stack_size(64 << 20);
    let ctx = Context::full(&rt).map_err(|e| e.to_string())?;
    ctx.with(|ctx| {
        let fail = |e: rquickjs::Error| match e {
            rquickjs::Error::Exception => format!("solver: {:?}", ctx.catch()),
            e => format!("solver: {e}"),
        };
        ctx.eval::<(), _>(inflate(EJS_LIB)?).map_err(fail)?;
        ctx.eval::<(), _>("globalThis.meriyah = lib.meriyah; globalThis.astring = lib.astring;").map_err(fail)?;
        ctx.eval::<(), _>(inflate(EJS_CORE)?).map_err(fail)?;
        ctx.globals().set("__player", player).map_err(fail)?;
        let out: Object = ctx
            .eval("jsc({type: 'player', player: __player, requests: [], output_preprocessed: true})")
            .map_err(fail)?;
        out.get::<_, String>("preprocessed_player").map_err(|_| "solver produced no output".to_string())
    })
}

fn inflate(data: &[u8]) -> Result<String, String> {
    let mut text = String::new();
    std::io::Read::read_to_string(&mut flate2::read::DeflateDecoder::new(data), &mut text).map_err(|e| e.to_string())?;
    Ok(text)
}

/// Compiles the preprocessed player to stripped QuickJS bytecode.
fn compile(code: &str) -> Result<Vec<u8>, String> {
    let rt = Runtime::new().map_err(|e| e.to_string())?;
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

    #[test]
    fn bundled_scripts_inflate_to_the_originals() {
        assert_eq!(inflate(EJS_LIB).unwrap(), include_str!("../../assets/ejs/yt.solver.lib.min.js"));
        assert_eq!(inflate(EJS_CORE).unwrap(), include_str!("../../assets/ejs/yt.solver.core.min.js"));
    }
}
