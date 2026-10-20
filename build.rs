//! Stores yt-dlp's solver scripts compressed in the binary. Only the
//! short-lived solver-builder process ever reads them, and only when the
//! native preprocessing fails, so they shouldn't cost 150 KB of exe.
//!
//! On Windows, also embeds the app manifest (assets/ytmcross.manifest) and,
//! with the GNU toolchain, the icon (assets/ytmcross.ico).

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
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_manifest(&out);
    }
}

/// The manifest opts into the segment heap: it hands freed memory back to
/// the OS more readily than the default heap (measured: -0.4 MB at idle).
fn embed_manifest(out: &std::path::Path) {
    let manifest = std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("assets/ytmcross.manifest");
    println!("cargo:rerun-if-changed=assets/ytmcross.manifest");
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo:rustc-link-arg-bin=ytmcross=/MANIFEST:EMBED");
        println!("cargo:rustc-link-arg-bin=ytmcross=/MANIFESTINPUT:{}", manifest.display());
        return;
    }
    // GNU: compile a resource script naming the manifest (RT_MANIFEST = 24),
    // the icon (ICON 1: Explorer, shortcuts, and the window via ui/shell.rs)
    // and the version info, whose description is the name Windows shows
    // (Task Manager, volume mixer, "Open with").
    let icon = manifest.with_file_name("ytmcross.ico");
    println!("cargo:rerun-if-changed=assets/ytmcross.ico");
    let path = |p: &std::path::Path| p.to_string_lossy().replace('\\', "/");
    let version = std::env::var("CARGO_PKG_VERSION").unwrap();
    let numbers = version.replace('.', ",");
    let rc = out.join("manifest.rc");
    std::fs::write(
        &rc,
        format!(
            "1 24 \"{}\"\n1 ICON \"{}\"\n1 VERSIONINFO\nFILEVERSION {numbers},0\nPRODUCTVERSION {numbers},0\n\
             BEGIN\n BLOCK \"StringFileInfo\"\n BEGIN\n  BLOCK \"040904B0\"\n  BEGIN\n\
             \x20  VALUE \"FileDescription\", \"YouTube Music\"\n   VALUE \"ProductName\", \"YouTube Music\"\n\
             \x20  VALUE \"FileVersion\", \"{version}\"\n   VALUE \"ProductVersion\", \"{version}\"\n\
             \x20  VALUE \"OriginalFilename\", \"ytmcross.exe\"\n  END\n END\n\
             \x20BLOCK \"VarFileInfo\"\n BEGIN\n  VALUE \"Translation\", 0x409, 1200\n END\nEND\n",
            path(&manifest),
            path(&icon)
        ),
    )
    .unwrap();
    let obj = out.join("manifest.o");
    let windres = std::env::var("WINDRES").unwrap_or_else(|_| "windres".into());
    match std::process::Command::new(&windres).arg(&rc).arg("-O").arg("coff").arg("-o").arg(&obj).status() {
        Ok(s) if s.success() => println!("cargo:rustc-link-arg-bin=ytmcross={}", obj.display()),
        _ => println!("cargo:warning=windres not found: building without the app manifest and icon"),
    }
}
