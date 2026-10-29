//! Kadence's Plasma front end.
//!
//! Startup is in C++ (`cpp/kadence.cpp`) because KAboutData, KCrash and
//! KLocalizedString have no Rust bindings; everything the QML talks to is a
//! Rust `QObject` (see [`bridge`]). Qt types stay inside this crate so a
//! second, non-Qt front end can reuse the same engine API ([`engine`]).

pub mod bridge;
mod demo;
pub mod engine;
mod ffi;
pub mod items;
pub mod project;
pub mod settings;
pub mod state;
mod test_helper;

/// Run the application with the process arguments. Returns the exit code.
pub fn run() -> i32 {
    // Make sure the linker keeps the QML module and its Rust types.
    cxx_qt::init_crate!(daw_app);
    cxx_qt::init_qml_module!("io.github.octodillo.daw");

    let args: Vec<String> = std::env::args().collect();
    ffi::run_application(&args, env!("CARGO_PKG_VERSION"))
}
