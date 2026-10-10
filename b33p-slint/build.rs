//! Compiles the UI. Slint fixes the widget style when it compiles a `.slint` file, and each
//! style is a whole copy of the UI in the binary, so b33p ships one: the native one, which
//! on Linux is Qt's (the desktop's Qt style, or Fusion; Qt picks between them at startup).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

fn main() {
    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("run by cargo"));
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("run by cargo"));
    let icons = write_icons(&out);
    let input = manifest.join("ui/app.slint");
    // Named outright on Linux: Slint resolves "native" through a file it expects at a fixed
    // place in `target/`, which newer cargo layouts move, and then quietly builds Fluent.
    let native = if std::env::var("CARGO_CFG_TARGET_OS").is_ok_and(|os| os == "linux") {
        "qt"
    } else {
        "native"
    };
    compile(&input, &out.join("app_native.rs"), native, false, &icons);
    // The UI tests (`src/ui_tests.rs`) run without Qt, so they get a Fluent copy, compiled
    // into the tests only. Finding elements in it needs debug info in its generated code.
    compile(&input, &out.join("app_fluent.rs"), "fluent", true, &icons);
    // The tray icon has no widgets, so its style doesn't matter.
    compile(
        &manifest.join("ui/tray.slint"),
        &out.join("tray.rs"),
        "native",
        false,
        &icons,
    );
}

/// Writes every Lucide icon to `$OUT_DIR/lucide/<name>.svg`; Slint embeds only those the UI
/// uses. All drawn on the same 24×24 grid, so equal boxes give icons a shared center.
fn write_icons(out: &Path) -> PathBuf {
    let dir = out.join("lucide");
    std::fs::create_dir_all(&dir).expect("create the icon directory");
    for icon in lucide_static_svg::Icon::all() {
        let path = dir.join(format!("{icon}.svg"));
        std::fs::write(&path, icon.svg_bytes()).expect("write an icon");
    }
    dir
}

fn compile(input: &Path, output: &Path, style: &str, debug_info: bool, icons: &Path) {
    let config = slint_build::CompilerConfiguration::new()
        .with_style(style.into())
        .with_debug_info(debug_info)
        .with_library_paths(HashMap::from([("lucide".to_owned(), icons.to_owned())]));
    let dependencies =
        slint_build::compile_with_output_path(input, output, config).unwrap_or_else(|error| {
            panic!(
                "failed to compile {} with the {style} style: {error}",
                input.display()
            )
        });
    for path in dependencies {
        // Not the icons written above: they change only with the crate, which reruns this.
        if !path.starts_with(icons) {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
}
