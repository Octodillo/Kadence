//! Scanning bundles in a child process.

use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, ExitStatus, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use daw_core::plugin::{PluginDescriptor, PluginFormatKind};

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
    #[error("the scanner crashed ({0})")]
    Crashed(ExitStatus),
    #[error("the scan took longer than {0:?}")]
    TimedOut(Duration),
    #[error("the scanner sent unreadable output: {0}")]
    BadOutput(String),
}

impl ScanError {
    /// Whether the failure says something about the plugin, and so belongs in the cache until the
    /// bundle changes. A scanner that could not start or ran out of time may do better next time.
    pub fn is_plugin_fault(&self) -> bool {
        matches!(self, Self::Crashed(_) | Self::BadOutput(_))
    }
}

/// Runs scans in child processes: `<program> <args>... --scan <format> <bundle>`.
#[derive(Clone, Debug)]
pub struct Scanner {
    program: PathBuf,
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
        Self {
            program: program.into(),
            args: args.into_iter().map(Into::into).collect(),
            timeout: Duration::from_secs(30),
            jobs: std::thread::available_parallelism().map_or(2, |n| n.get().min(8)),
        }
    }

    /// A scanner that runs the current executable, for applications that forward `--scan` to
    /// [`run_scan_child`].
    pub fn current_exe() -> std::io::Result<Self> {
        Ok(Self::new(std::env::current_exe()?, Vec::<OsString>::new()))
    }

    /// A scanner that runs the `daw-plugin-scan` binary installed next to the current executable.
    pub fn sibling() -> std::io::Result<Self> {
        let exe = std::env::current_exe()?;
        let dir = exe.parent().unwrap_or(Path::new("."));
        Ok(Self::new(
            dir.join("daw-plugin-scan"),
            Vec::<OsString>::new(),
        ))
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
    pub fn scan(
        &self,
        kind: &PluginFormatKind,
        bundle: &Path,
    ) -> Result<Vec<PluginDescriptor>, ScanError> {
        let mut child = Command::new(&self.program)
            .args(&self.args)
            .arg(SCAN_ARG)
            .arg(kind_name(kind))
            .arg(bundle)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(ScanError::Spawn)?;

        // Read on a thread so a chatty child cannot block on a full pipe. The result comes back
        // over a channel so that waiting for it can time out: a process the plugin started may
        // inherit the pipe and keep it open after the scanner exits.
        let mut stdout = child.stdout.take().expect("stdout is piped");
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut out = Vec::new();
            let _ = stdout.read_to_end(&mut out);
            let _ = tx.send(out);
        });

        let start = Instant::now();
        let status = loop {
            match child.try_wait().map_err(ScanError::Spawn)? {
                Some(status) => break status,
                None if start.elapsed() >= self.timeout => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(ScanError::TimedOut(self.timeout));
                }
                None => std::thread::sleep(Duration::from_millis(5)),
            }
        };
        if !status.success() {
            return Err(ScanError::Crashed(status));
        }
        let remaining = self.timeout.saturating_sub(start.elapsed());
        let out = rx
            .recv_timeout(remaining.max(Duration::from_millis(500)))
            .map_err(|_| ScanError::TimedOut(self.timeout))?;
        serde_json::from_slice(&out).map_err(|e| ScanError::BadOutput(e.to_string()))
    }
}

/// The child side of a scan. If `args` (without the program name) are
/// `--scan <format> <bundle>`, scans the bundle with `registry`'s format, prints the descriptors
/// as JSON to stdout and returns `Some` exit code. Otherwise returns `None`, so an application
/// can call this first thing in `main` and carry on normally when it is not a scan.
pub fn run_scan_child(
    registry: &PluginRegistry,
    args: impl IntoIterator<Item = OsString>,
) -> Option<ExitCode> {
    let args: Vec<OsString> = args.into_iter().collect();
    let pos = args.iter().position(|a| a == SCAN_ARG)?;
    let (Some(kind), Some(bundle)) = (args.get(pos + 1), args.get(pos + 2)) else {
        eprintln!("usage: {SCAN_ARG} <clap|lv2|vst3> <bundle>");
        return Some(ExitCode::from(2));
    };
    let Some(format) = kind
        .to_str()
        .and_then(kind_from_name)
        .and_then(|k| registry.format(&k))
    else {
        eprintln!("unsupported plugin format {kind:?}");
        return Some(ExitCode::from(2));
    };
    // Plugins may print to stdout while loading. Keep the real stdout for the result and point
    // file descriptor 1 at stderr for the duration of the scan.
    let Some(mut result_out) = take_stdout() else {
        return Some(ExitCode::FAILURE);
    };
    let found = format.scan(Path::new(bundle));
    let json = serde_json::to_vec(&found).expect("descriptors serialize");
    if result_out
        .write_all(&json)
        .and_then(|()| result_out.flush())
        .is_err()
    {
        return Some(ExitCode::FAILURE);
    }
    Some(ExitCode::SUCCESS)
}

/// Moves stdout to a new file descriptor, returned as a file, and makes descriptor 1 a copy of
/// stderr.
fn take_stdout() -> Option<std::fs::File> {
    use std::os::fd::FromRawFd;
    let _ = std::io::stdout().flush();
    // SAFETY: plain descriptor duplication; `saved` is a fresh descriptor this function owns.
    unsafe {
        let saved = libc::dup(libc::STDOUT_FILENO);
        if saved < 0 || libc::dup2(libc::STDERR_FILENO, libc::STDOUT_FILENO) < 0 {
            return None;
        }
        Some(std::fs::File::from_raw_fd(saved))
    }
}
