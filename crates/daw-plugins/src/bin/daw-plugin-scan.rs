//! The smallest scanner: `daw-plugin-scan --scan <format> <bundle>` scans one plugin bundle and
//! prints what it found as JSON (see [`daw_plugins::run_scan_child`]). Kadence itself and
//! `daw-plugin-host` answer `--scan` the same way and are what an installation runs; this
//! binary lets the registry's tests scan without either.

use std::process::ExitCode;

fn main() -> ExitCode {
    let registry = daw_plugins::default_registry();
    daw_plugins::run_scan_child(&registry, std::env::args_os().skip(1)).unwrap_or_else(|| {
        eprintln!("usage: daw-plugin-scan --scan <clap|lv2|vst3> <bundle>");
        ExitCode::from(daw_plugins::EXIT_UNSUPPORTED)
    })
}
