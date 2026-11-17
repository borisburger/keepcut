//! Windows: link against third_party/mpv/mpv.lib (made by packaging/windows/fetch-mpv.ps1)
//! and copy libmpv-2.dll next to the built executable. Elsewhere the system libmpv is used.

use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let mpv_dir = manifest.join("../../third_party/mpv");
    let mpv_dir = mpv_dir.canonicalize().unwrap_or(mpv_dir);
    if !mpv_dir.join("mpv.lib").is_file() {
        panic!(
            "libmpv not found in {}. Run: powershell -ExecutionPolicy Bypass -File packaging/windows/fetch-mpv.ps1",
            mpv_dir.display()
        );
    }
    println!("cargo:rustc-link-search=native={}", mpv_dir.display());
    println!(
        "cargo:rerun-if-changed={}",
        mpv_dir.join("libmpv-2.dll").display()
    );

    // OUT_DIR is somewhere below target/<profile>/build/ (the depth differs between cargo
    // versions), so look for the <profile> directory itself.
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let profile = std::env::var("PROFILE").unwrap_or_else(|_| "debug".into());
    let profile_dir = out
        .ancestors()
        .find(|p| p.file_name().is_some_and(|n| n == profile.as_str()) && p.join("build").is_dir());
    if let Some(profile_dir) = profile_dir {
        let dst = profile_dir.join("libmpv-2.dll");
        let src = mpv_dir.join("libmpv-2.dll");
        let stale = match (std::fs::metadata(&src), std::fs::metadata(&dst)) {
            (Ok(s), Ok(d)) => s.len() != d.len() || s.modified().ok() > d.modified().ok(),
            _ => true,
        };
        if stale {
            std::fs::copy(&src, &dst).expect("copying libmpv-2.dll");
        }
    }
}
