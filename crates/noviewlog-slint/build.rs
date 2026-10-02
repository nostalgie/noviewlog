fn main() {
    // Prefer cargo:rustc-link-search over RUSTFLAGS=-L: env rustflags change
    // the fingerprint of every crate and force a full rebuild when the run
    // script and a plain `cargo build` disagree.
    let manifest_dir = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let deps_lib = manifest_dir.join("../../.deps/lib");
    if let Ok(deps_lib) = deps_lib.canonicalize() {
        println!("cargo:rustc-link-search=native={}", deps_lib.display());
    }

    // fluent supports Palette.color-scheme (dark/light). Lock look via dark in app.slint init;
    // light theme later = set ColorScheme.light without changing this style name.
    let config = slint_build::CompilerConfiguration::new().with_style("fluent".into());
    slint_build::compile_with_config("ui/app.slint", config).expect("Slint UI compile failed");

    // Embed the app icon as a PE resource so Explorer, shortcuts, the taskbar
    // and the window caption show it. Windows only; skip for other targets.
    embed_windows_resource();
}

// The build script is compiled for the HOST, so winresource (a
// cfg(windows) build-dependency) must stay out of Linux/macOS builds;
// reference it only under #[cfg(windows)]. The runtime CARGO_CFG_TARGET_OS
// check keeps Windows-host cross builds safe.
#[cfg(windows)]
fn embed_windows_resource() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set_icon("ui/assets/app.ico");
    res.compile()
        .expect("Windows resource (app.ico) embed failed");
}

#[cfg(not(windows))]
fn embed_windows_resource() {}
