//! Where diagnostics go.
//!
//! Library crates report through the [`log`] macros (`log::warn!` and so on)
//! and never print. Each binary calls [`init`] first thing in `main`, which
//! sends the records to stderr; a desktop session's journal collects that.
//! Until `init` runs, records go nowhere.
//!
//! Formatting and writing a record allocates and can block, so nothing on the
//! audio thread may log. Real-time code counts what went wrong in an atomic
//! and the control side logs the count.
//!
//! What to use each level for:
//! - `error`: something the user asked for did not happen, or data was lost.
//! - `warn`: Kadence carried on, but not as asked (a refused buffer size, a
//!   connection that could not be restored).
//! - `info`, `debug`: detail for someone chasing a problem. Hidden unless
//!   [`LEVEL_ENV`] asks for it.

use std::io::Write;
use std::sync::OnceLock;

use log::{LevelFilter, Log, Metadata, Record};

/// Environment variable naming the most detailed level to print: `off`,
/// `error`, `warn` (the default), `info`, `debug` or `trace`.
pub const LEVEL_ENV: &str = "KADENCE_LOG";

const DEFAULT_LEVEL: LevelFilter = LevelFilter::Warn;

/// Sends every crate's `log` records to stderr, each line starting with
/// `program` so the application's lines can be told from those of the plugin
/// host and scanner processes it starts. The level comes from [`LEVEL_ENV`].
///
/// Call once, first thing in `main`. Later calls change nothing.
pub fn init(program: &'static str) {
    static LOGGER: OnceLock<StderrLogger> = OnceLock::new();
    let logger = LOGGER.get_or_init(|| StderrLogger { program });
    // Fails only when a logger is already installed, which then stays.
    if log::set_logger(logger).is_ok() {
        let level = std::env::var(LEVEL_ENV).ok();
        log::set_max_level(parse_level(level.as_deref()));
    }
}

/// The level `value` names, or the default when it is unset or not a level.
fn parse_level(value: Option<&str>) -> LevelFilter {
    value
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(DEFAULT_LEVEL)
}

struct StderrLogger {
    program: &'static str,
}

impl StderrLogger {
    fn line(&self, record: &Record) -> String {
        format!(
            "{}: {}: {}: {}",
            self.program,
            record.level().as_str().to_ascii_lowercase(),
            record.target(),
            record.args()
        )
    }
}

impl Log for StderrLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= log::max_level()
    }

    fn log(&self, record: &Record) {
        if self.enabled(record.metadata()) {
            // Nothing useful can be done when stderr is closed or full.
            let _ = writeln!(std::io::stderr().lock(), "{}", self.line(record));
        }
    }

    fn flush(&self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_level_defaults_to_warnings_and_reads_any_case() {
        assert_eq!(parse_level(None), LevelFilter::Warn);
        assert_eq!(parse_level(Some("")), LevelFilter::Warn);
        assert_eq!(parse_level(Some("nonsense")), LevelFilter::Warn);
        assert_eq!(parse_level(Some("debug")), LevelFilter::Debug);
        assert_eq!(parse_level(Some(" INFO ")), LevelFilter::Info);
        assert_eq!(parse_level(Some("off")), LevelFilter::Off);
    }

    #[test]
    fn a_line_names_the_program_level_and_origin() {
        let logger = StderrLogger { program: "kadence" };
        let line = logger.line(
            &Record::builder()
                .level(log::Level::Warn)
                .target("daw_backend::jack_backend")
                .args(format_args!("JACK refused buffer size {}", 64))
                .build(),
        );
        assert_eq!(
            line,
            "kadence: warn: daw_backend::jack_backend: JACK refused buffer size 64"
        );
    }
}
