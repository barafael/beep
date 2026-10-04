//! Native Windows frontend: WinUI 3 through windows-reactor's Elm-style components.

#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
mod app;

#[cfg(windows)]
fn main() {
    if let Err(error) = windows_reactor::App::run_component::<app::Langis>(()) {
        eprintln!("langis failed to start: {error}");
        std::process::exit(1);
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("langis-win is Windows-only (WinUI 3). Use `cargo run -p langis-slint` instead.");
    std::process::exit(1);
}
