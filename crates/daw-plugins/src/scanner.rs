//! Scanning bundles in a child process.

use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use daw_core::plugin::{PluginDescriptor, PluginFormatKind};

use crate::registry::PluginRegistry;

/// The argument that selects scan mode in the child.
const SCAN_ARG: &str = "--scan";

pub fn kind_name(kind: &PluginFormatKind) -> &'static str {
    match kind {
        PluginFormatKind::Clap => "clap",
        PluginFormatKind::Lv2 => "lv2",
        PluginFormatKind::Vst3 => "vst3",
    }
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

        // Read on a thread so a chatty child cannot block on a full pipe.
        let mut stdout = child.stdout.take().expect("stdout is piped");
        let reader = std::thread::spawn(move || {
            let mut out = Vec::new();
            let _ = stdout.read_to_end(&mut out);
            out
        });

        let start = Instant::now();
        let status = loop {
            match child.try_wait().map_err(ScanError::Spawn)? {
                Some(status) => break status,
                None if start.elapsed() >= self.timeout => {
                    let _ = child.kill();
                    let _ = child.wait();
                    // Not joined: a process the plugin forked may still hold the pipe open.
                    drop(reader);
                    return Err(ScanError::TimedOut(self.timeout));
                }
                None => std::thread::sleep(Duration::from_millis(5)),
            }
        };
        let out = reader.join().unwrap_or_default();
        if !status.success() {
            return Err(ScanError::Crashed(status));
        }
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
    let found = format.scan(Path::new(bundle));
    let json = serde_json::to_vec(&found).expect("descriptors serialize");
    let mut stdout = std::io::stdout().lock();
    if stdout
        .write_all(&json)
        .and_then(|()| stdout.flush())
        .is_err()
    {
        return Some(ExitCode::FAILURE);
    }
    Some(ExitCode::SUCCESS)
}
