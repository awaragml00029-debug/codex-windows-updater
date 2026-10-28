fn main() {
    slint_build::compile("ui/app.slint").expect("Slint build failed");

    // Check the *target*, not the host: build scripts run on the host, so
    // `#[cfg(windows)]` is false when cross-compiling from Linux and the
    // manifest (Common-Controls v6, DPI, UTF-8 code page) would be dropped.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_manifest_file("app.manifest");
        // Icon is embedded later when we have one; placeholder only for now.
        if std::path::Path::new("assets/installer.ico").exists() {
            res.set_icon("assets/installer.ico");
        }
        res.compile().expect("Windows resource embed failed");
    }
}
