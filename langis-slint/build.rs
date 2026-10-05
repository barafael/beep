//! Compiles the UI once per Slint widget style. Slint fixes the style when it compiles a
//! `.slint` file, so offering several in the settings means building each; the `styles` module
//! in `src/main.rs` includes them, and the app switches between them at runtime.

use std::path::PathBuf;

/// Slint styles, each compiled into `$OUT_DIR/app_<style>.rs`. `native` is Slint's choice for
/// the platform (Qt on Linux).
const STYLES: [&str; 5] = ["native", "fluent", "material", "cupertino", "cosmic"];

fn main() {
    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("run by cargo"));
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("run by cargo"));
    let input = manifest.join("ui/app.slint");
    // The UI tests (`src/ui_tests.rs`) find elements in the fluent window, which needs debug
    // info in its generated code; only there, and only in debug builds.
    let debug = std::env::var("PROFILE").is_ok_and(|profile| profile == "debug");
    for style in STYLES {
        let config = slint_build::CompilerConfiguration::new()
            .with_style(style.into())
            .with_debug_info(debug && style == "fluent");
        let dependencies = slint_build::compile_with_output_path(
            &input,
            out.join(format!("app_{style}.rs")),
            config,
        )
        .unwrap_or_else(|error| {
            panic!("failed to compile ui/app.slint with the {style} style: {error}")
        });
        for path in dependencies {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
    // The tray icon has no widgets, so one build serves every style.
    let dependencies = slint_build::compile_with_output_path(
        manifest.join("ui/tray.slint"),
        out.join("tray.rs"),
        slint_build::CompilerConfiguration::new(),
    )
    .unwrap_or_else(|error| panic!("failed to compile ui/tray.slint: {error}"));
    for path in dependencies {
        println!("cargo:rerun-if-changed={}", path.display());
    }
}
