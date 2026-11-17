//! The plugin sandbox host: `daw-plugin-host --plugin-host <socket>` loads one plugin and runs
//! it for the process that opened `<socket>`. It also scans, like `daw-plugin-scan`:
//! `daw-plugin-host --scan <format> <bundle>`.

use std::process::ExitCode;

fn main() -> ExitCode {
    let registry = daw_plugins::default_registry();
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    daw_plugin_host::run_host_child(&registry, args.iter().cloned())
        .or_else(|| daw_plugins::run_scan_child(&registry, args))
        .unwrap_or_else(|| {
            eprintln!(
                "usage: daw-plugin-host --plugin-host <socket>\n       daw-plugin-host --scan <clap|lv2|vst3> <bundle>"
            );
            ExitCode::from(2)
        })
}
