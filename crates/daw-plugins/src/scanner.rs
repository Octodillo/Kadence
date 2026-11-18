//! Scanning bundles in a child process.

use std::ffi::OsString;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, ExitStatus, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use daw_core::plugin::{BundleScan, PluginFormatKind};
use serde::{Deserialize, Serialize};

use crate::registry::PluginRegistry;

/// The argument that selects scan mode in the child.
const SCAN_ARG: &str = "--scan";

/// The name of `kind` on the scanner's command line, which is also its bundle extension.
pub fn kind_name(kind: &PluginFormatKind) -> &'static str {
    crate::candidates::bundle_extension(kind)
}

pub fn kind_from_name(name: &str) -> Option<PluginFormatKind> {
    match name {
        "clap" => Some(PluginFormatKind::Clap),
        "lv2" => Some(PluginFormatKind::Lv2),
        "vst3" => Some(PluginFormatKind::Vst3),
        _ => None,
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ScanError {
    #[error("could not start the scanner: {0}")]
    Spawn(std::io::Error),
    /// The scanner has no program to run; see [`Scanner::unavailable`].
    #[error("plugins cannot be scanned: {0}")]
    Unavailable(String),
    #[error("the scanner crashed ({0})")]
    Crashed(ExitStatus),
    #[error("the scan took longer than {0:?}")]
    TimedOut(Duration),
    #[error("the scanner sent unreadable output: {0}")]
    BadOutput(String),
    #[error("the scanner does not support this format")]
    Unsupported,
    /// The bundle did not load: a missing library, no entry point, a factory that failed.
    #[error("the plugin did not load: {0}")]
    LoadFailed(String),
}

impl ScanError {
    /// Whether the failure says something about the plugin, and so belongs in the cache until the
    /// bundle changes. A scanner that could not start, or was built without the format, may do
    /// better next time. Hangs count as the plugin's fault so that a plugin which always hangs
    /// does not cost the full timeout on every launch; [`crate::ScanCache::forget_failures`]
    /// retries them.
    pub fn is_plugin_fault(&self) -> bool {
        matches!(
            self,
            Self::Crashed(_) | Self::BadOutput(_) | Self::TimedOut(_) | Self::LoadFailed(_)
        )
    }
}

/// Runs scans in child processes: `<program> <args>... --scan <format> <bundle>`.
#[derive(Clone, Debug)]
pub struct Scanner {
    /// The program to run, or why there is none.
    program: Result<PathBuf, String>,
    args: Vec<OsString>,
    timeout: Duration,
    jobs: usize,
}

impl Scanner {
    /// A scanner that runs `program` with `args` before the scan arguments.
    pub fn new(
        program: impl Into<PathBuf>,
        args: impl IntoIterator<Item = impl Into<OsString>>,
    ) -> Self {
        Self::with_program(
            Ok(program.into()),
            args.into_iter().map(Into::into).collect(),
        )
    }

    fn with_program(program: Result<PathBuf, String>, args: Vec<OsString>) -> Self {
        Self {
            program,
            args,
            timeout: Duration::from_secs(30),
            jobs: std::thread::available_parallelism().map_or(2, |n| n.get().min(8)),
        }
    }

    /// A scanner that runs the current executable, for applications that forward `--scan` to
    /// [`run_scan_child`].
    ///
    /// When the executable's path cannot be read (no `/proc`, say) the scanner is
    /// [`unavailable`](Self::unavailable). Running a program of the same name from `PATH`
    /// instead could run another version, or anything a writable `PATH` entry holds.
    pub fn current_exe() -> Self {
        match std::env::current_exe() {
            Ok(exe) => Self::new(exe, Vec::<OsString>::new()),
            Err(e) => Self::unavailable(format!("this program cannot find itself: {e}")),
        }
    }

    /// A scanner with no program to run: every [`scan`](Self::scan) fails with
    /// [`ScanError::Unavailable`] and `why`.
    ///
    /// A registry scanning with it still lists the bundles its cache answers, which need no
    /// scanner. Each bundle that is new or changed is reported as failed with `why`, and that
    /// is never cached, so it is scanned once there is a scanner.
    pub fn unavailable(why: impl Into<String>) -> Self {
        Self::with_program(Err(why.into()), Vec::new())
    }

    /// How long one bundle may take before its scanner is killed. Default 30 s.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// How many scanners run at once. Default: the number of CPUs, at most 8.
    pub fn with_jobs(mut self, jobs: usize) -> Self {
        self.jobs = jobs.max(1);
        self
    }

    pub fn jobs(&self) -> usize {
        self.jobs
    }

    /// Scans one bundle in a child process.
    pub fn scan(&self, kind: &PluginFormatKind, bundle: &Path) -> Result<BundleScan, ScanError> {
        let program = self
            .program
            .as_ref()
            .map_err(|why| ScanError::Unavailable(why.clone()))?;
        let mut command = Command::new(program);
        command
            .args(&self.args)
            .arg(SCAN_ARG)
            .arg(kind_name(kind))
            .arg(bundle)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        // Any helpers the plugin starts are killed with the scanner, however it ends.
        crate::process::in_own_group(&mut command);
        // Out of the terminal's process group the scanner no longer sees Ctrl-C, so tie its
        // life to this thread instead, which always outlives it.
        #[cfg(target_os = "linux")]
        // SAFETY: `prctl` is async-signal-safe and touches no memory.
        unsafe {
            command.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
                Ok(())
            });
        }
        let mut child = command.spawn().map_err(ScanError::Spawn)?;

        // Read on a thread so a chatty child cannot block on a full pipe. A process the plugin
        // started may inherit the pipe and hold it open after the scanner exits, so the bytes are
        // shared as they arrive and EOF is only waited for briefly.
        let mut stdout = child.stdout.take().expect("stdout is piped");
        let out = Arc::new(Mutex::new(Vec::new()));
        let (eof_tx, eof_rx) = mpsc::channel();
        {
            let out = Arc::clone(&out);
            std::thread::spawn(move || {
                let mut chunk = [0u8; 4096];
                while let Ok(n @ 1..) = stdout.read(&mut chunk) {
                    out.lock().unwrap().extend_from_slice(&chunk[..n]);
                }
                let _ = eof_tx.send(());
            });
        }

        let start = Instant::now();
        let timed_out = loop {
            if crate::process::has_exited(&child).map_err(ScanError::Spawn)? {
                break false;
            }
            if start.elapsed() >= self.timeout {
                break true;
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        // Before the wait, while the group's id cannot have been reused.
        crate::process::kill_group(&child);
        let status = child.wait().map_err(ScanError::Spawn)?;
        // The exit status, or `None` for a scanner killed at the timeout.
        let status = (!timed_out).then_some(status);
        if status.and_then(|s| s.code()) == Some(i32::from(EXIT_UNSUPPORTED)) {
            return Err(ScanError::Unsupported);
        }
        // The scanner is gone, so everything it wrote is in the pipe; whether EOF arrives
        // depends only on other holders of the pipe.
        let _ = eof_rx.recv_timeout(Duration::from_millis(500));
        let out = out.lock().unwrap();
        // The scanner writes its result in one piece once the scan is over. Plugins often crash
        // or hang after that, in code that runs as the process exits, so a complete result
        // counts however the scanner ended.
        match (serde_json::from_slice(&out), status) {
            (Ok(ScanOutput::Found(found)), _) => Ok(found),
            (Ok(ScanOutput::Failed(f)), _) => Err(ScanError::LoadFailed(f.load_failed)),
            (Err(_), None) => Err(ScanError::TimedOut(self.timeout)),
            (Err(e), Some(status)) if exited_normally(status) => {
                Err(ScanError::BadOutput(e.to_string()))
            }
            (Err(_), Some(status)) => Err(ScanError::Crashed(status)),
        }
    }
}

/// Whether the scanner ended with one of its own exit codes, not a crash.
fn exited_normally(status: ExitStatus) -> bool {
    status.success() || status.code() == Some(i32::from(EXIT_LOAD_FAILED))
}

/// The scanner's exit code when it cannot scan at all: bad arguments, a format it was built
/// without, or no way to set up its output. Not the plugin's fault, so not cached.
pub const EXIT_UNSUPPORTED: u8 = 64;

/// The scanner's exit code when the bundle did not load. Its output is a [`LoadFailure`].
const EXIT_LOAD_FAILED: u8 = 65;

/// What the scanner writes when the bundle did not load.
#[derive(Serialize, Deserialize)]
struct LoadFailure {
    load_failed: String,
}

/// What the scanner writes: what it found, or why the bundle did not load.
#[derive(Deserialize)]
#[serde(untagged)]
enum ScanOutput {
    Found(BundleScan),
    Failed(LoadFailure),
}

/// The child side of a scan. If `args` (without the program name) are
/// `--scan <format> <bundle>`, scans the bundle with `registry`'s format and prints the
/// [`BundleScan`] as JSON to stdout; a bundle that does not load prints
/// `{"load_failed": <reason>}` instead, with exit code 65. Otherwise returns `None`, so an
/// application can call this first thing in `main` and carry on normally when it is not a scan.
///
/// Once the result is written the process ends here, without running exit-time code: the
/// plugin's library is still loaded, and its static destructors and `atexit` handlers often
/// crash or hang. `Some` exit code comes back only when there was no scan to report: bad
/// arguments, a format this build lacks, or a result that could not be written.
pub fn run_scan_child(
    registry: &PluginRegistry,
    args: impl IntoIterator<Item = OsString>,
) -> Option<ExitCode> {
    let args: Vec<OsString> = args.into_iter().collect();
    let pos = args.iter().position(|a| a == SCAN_ARG)?;
    // This process is a scanner, whatever binary it was started from. The
    // application starts scans with stderr closed (plugins print a lot while
    // loading), so these lines are for a scan run by hand.
    daw_core::logging::init("kadence-plugin-scan");
    let (Some(kind), Some(bundle)) = (args.get(pos + 1), args.get(pos + 2)) else {
        log::error!("usage: {SCAN_ARG} <clap|lv2|vst3> <bundle>");
        return Some(ExitCode::from(EXIT_UNSUPPORTED));
    };
    let Some(format) = kind
        .to_str()
        .and_then(kind_from_name)
        .and_then(|k| registry.format(&k))
    else {
        log::error!("unsupported plugin format {kind:?}");
        return Some(ExitCode::from(EXIT_UNSUPPORTED));
    };
    // Plugins may print to stdout while loading. Point file descriptor 1 at stderr for the
    // scan and write the result to the real stdout.
    let Some(redirect) = StdoutRedirect::to_stderr() else {
        return Some(ExitCode::from(EXIT_UNSUPPORTED));
    };
    let (json, code) = match format.scan(Path::new(bundle)) {
        Ok(found) => (serde_json::to_vec(&found).expect("a scan serializes"), 0),
        Err(e) => {
            let failure = LoadFailure {
                load_failed: e.reason(),
            };
            (
                serde_json::to_vec(&failure).expect("a string serializes"),
                EXIT_LOAD_FAILED,
            )
        }
    };
    if redirect.write_result(&json).is_err() {
        return Some(ExitCode::FAILURE);
    }
    // SAFETY: `_exit` ends the process at once. The result is written (straight to the
    // descriptor, so nothing is left in a buffer), and nothing else here needs to run first.
    unsafe { libc::_exit(i32::from(code)) }
}

/// Holds the real stdout while descriptor 1 points at stderr. Dropping it restores stdout,
/// which a scan that reports its result never gets to: it exits first.
struct StdoutRedirect {
    saved: OwnedFd,
}

impl StdoutRedirect {
    fn to_stderr() -> Option<Self> {
        let _ = std::io::stdout().flush();
        // Close-on-exec, so programs the plugin starts do not inherit the result pipe.
        // SAFETY: `F_DUPFD_CLOEXEC` returns a fresh descriptor (or -1), which `OwnedFd` then owns
        // and closes.
        let saved = unsafe {
            let fd = libc::fcntl(libc::STDOUT_FILENO, libc::F_DUPFD_CLOEXEC, 0);
            if fd < 0 {
                return None;
            }
            OwnedFd::from_raw_fd(fd)
        };
        // SAFETY: both descriptors are open; `dup2` only replaces descriptor 1.
        if unsafe { libc::dup2(libc::STDERR_FILENO, libc::STDOUT_FILENO) } < 0 {
            return None;
        }
        Some(Self { saved })
    }

    fn write_result(&self, data: &[u8]) -> std::io::Result<()> {
        let mut out = std::fs::File::from(self.saved.try_clone()?);
        out.write_all(data)
    }
}

impl Drop for StdoutRedirect {
    fn drop(&mut self) {
        let _ = std::io::stdout().flush();
        // SAFETY: `saved` is open for the lifetime of `self`; this puts it back as descriptor 1.
        unsafe {
            libc::dup2(self.saved.as_raw_fd(), libc::STDOUT_FILENO);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_saved_stdout_is_not_inherited_by_helpers() {
        let redirect = StdoutRedirect::to_stderr().unwrap();
        // SAFETY: `saved` is an open descriptor owned by `redirect`.
        let flags = unsafe { libc::fcntl(redirect.saved.as_raw_fd(), libc::F_GETFD) };
        drop(redirect);
        assert!(flags >= 0 && flags & libc::FD_CLOEXEC != 0, "flags {flags}");
    }
}
