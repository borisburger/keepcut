//! Windows: embed the app icon and version information in keepcut.exe.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../packaging/icons/keepcut.ico");
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../../packaging/icons/keepcut.ico")
            .set("ProductName", "Keepcut")
            .set("FileDescription", "Keepcut")
            .set(
                "LegalCopyright",
                "Copyright © Boris Burger. GPL-3.0-or-later",
            );
        if let Err(e) = res.compile() {
            // A missing resource compiler only costs the icon; don't fail the build.
            println!("cargo:warning=could not embed the Windows resources: {e}");
        }
    }
}
