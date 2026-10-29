//! Scans one plugin bundle and prints its descriptors as JSON: `daw-plugin-scan --scan <format>
//! <bundle>`. The registry runs this in a child process so that a crashing plugin only takes
//! this process down.

use std::process::ExitCode;

fn main() -> ExitCode {
    let registry = daw_plugins::default_registry();
    daw_plugins::run_scan_child(&registry, std::env::args_os().skip(1)).unwrap_or_else(|| {
        eprintln!("usage: daw-plugin-scan --scan <clap|lv2|vst3> <bundle>");
        ExitCode::from(2)
    })
}
