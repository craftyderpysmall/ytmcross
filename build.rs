//! Stores yt-dlp's solver scripts compressed in the binary. Only the
//! short-lived solver-builder process ever reads them, and only when the
//! native preprocessing fails, so they shouldn't cost 150 KB of exe.

use std::io::Write;

fn main() {
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    for name in ["yt.solver.lib.min.js", "yt.solver.core.min.js"] {
        let src = format!("assets/ejs/{name}");
        println!("cargo:rerun-if-changed={src}");
        let text = std::fs::read(&src).expect("solver script");
        let mut enc = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::best());
        enc.write_all(&text).unwrap();
        std::fs::write(out.join(format!("{name}.deflate")), enc.finish().unwrap()).unwrap();
    }
}
