//! Kadence's Plasma front end.
//!
//! Startup is in C++ (`cpp/kadence.cpp`) because KAboutData, KCrash and
//! KLocalizedString have no Rust bindings; everything the QML talks to is a
//! Rust `QObject` (see [`bridge`]). Qt types stay inside this crate so a
//! second, non-Qt front end can reuse the same engine API ([`engine`]).

pub mod arrange_view;
pub mod bridge;
mod demo;
pub mod engine;
mod ffi;
pub mod instrument;
pub mod instruments_bridge;
pub mod message;
pub mod mixer;
pub mod mixer_bridge;
mod paint;
pub mod piano_roll;
pub mod project;
mod recording;
pub mod session_mixer;
pub mod settings;
pub mod settings_bridge;
pub mod state;
mod test_helper;

/// Runs a plugin sandbox host or scanner if the arguments ask for one
/// (`--plugin-host <socket>` or `--scan <format> <bundle>`), which the
/// session's plugin loader starts this executable with. Call it first thing,
/// before Qt sees the arguments. `None` means start the application.
pub fn run_plugin_child() -> Option<std::process::ExitCode> {
    let registry = daw_plugins::default_registry();
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    daw_plugin_host::run_host_child(&registry, args.iter().cloned())
        .or_else(|| daw_plugins::run_scan_child(&registry, args))
}

/// Run the application with the process arguments. Returns the exit code.
pub fn run() -> i32 {
    // Make sure the linker keeps the QML module and its Rust types.
    cxx_qt::init_crate!(daw_app);
    cxx_qt::init_qml_module!("io.github.octodillo.daw");

    use std::os::unix::ffi::OsStrExt;

    let mut args = Vec::new();
    for arg in std::env::args_os() {
        args.extend_from_slice(arg.as_bytes());
        args.push(0);
    }
    ffi::run_application(&args, env!("CARGO_PKG_VERSION"))
}
