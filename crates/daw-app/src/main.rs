//! `kadence`: see the `daw_app` library.

use std::process::ExitCode;

fn main() -> ExitCode {
    // A plugin sandbox or scanner the app started: no Qt there.
    if let Some(code) = daw_app::run_plugin_child() {
        return code;
    }
    std::process::exit(daw_app::run());
}
