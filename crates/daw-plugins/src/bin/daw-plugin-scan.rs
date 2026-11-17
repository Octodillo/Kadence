//! Scans one plugin bundle and prints its descriptors as JSON: `daw-plugin-scan --scan <format>
//! <bundle>`. The registry runs this in a child process so that a crashing plugin only takes
//! this process down.

use std::process::ExitCode;

fn main() -> ExitCode {
    let registry = daw_plugins::default_registry();
    match daw_plugins::run_scan_child(&registry, std::env::args_os().skip(1)) {
        // The result is written. Leave without running exit-time code, such as static
        // destructors of plugin libraries that are still loaded, which often crash.
        // SAFETY: `_exit` ends the process at once; nothing needs to run first.
        Some(code) if code == ExitCode::SUCCESS => unsafe { libc::_exit(0) },
        // Likewise when the bundle did not load (65): the reason is written.
        // SAFETY: as above.
        Some(code) if code == ExitCode::from(65) => unsafe { libc::_exit(65) },
        Some(code) => code,
        None => {
            eprintln!("usage: daw-plugin-scan --scan <clap|lv2|vst3> <bundle>");
            ExitCode::from(64)
        }
    }
}
