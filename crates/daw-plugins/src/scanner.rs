//! Scanning bundles in a child process.

use std::ffi::OsString;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, ExitStatus, Stdio};
use std::sync::{Arc, Mutex, mpsc};
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
    #[error("the scanner does not support this format")]
    Unsupported,
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
            Self::Crashed(_) | Self::BadOutput(_) | Self::TimedOut(_)
        )
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
        let mut command = Command::new(&self.program);
        command
            .args(&self.args)
            .arg(SCAN_ARG)
            .arg(kind_name(kind))
            .arg(bundle)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            // Its own process group, so a timeout also kills any helpers the plugin started.
            .process_group(0);
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
        let status = loop {
            match child.try_wait().map_err(ScanError::Spawn)? {
                Some(status) => break status,
                None if start.elapsed() >= self.timeout => {
                    if let Ok(pid) = libc::pid_t::try_from(child.id()) {
                        // SAFETY: plain syscall; the child is unreaped, so its group id is ours.
                        unsafe { libc::kill(-pid, libc::SIGKILL) };
                    }
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(ScanError::TimedOut(self.timeout));
                }
                None => std::thread::sleep(Duration::from_millis(5)),
            }
        };
        if status.code() == Some(i32::from(EXIT_UNSUPPORTED)) {
            return Err(ScanError::Unsupported);
        }
        // The scanner has exited, so everything it wrote is in the pipe; whether EOF arrives
        // depends only on other holders of the pipe.
        let _ = eof_rx.recv_timeout(Duration::from_millis(500));
        let out = out.lock().unwrap();
        // Plugins often crash at exit, in destructors that run after the result was written, so
        // a complete result counts even from a scanner that crashed.
        match serde_json::from_slice(&out) {
            Ok(found) => Ok(found),
            Err(e) if status.success() => Err(ScanError::BadOutput(e.to_string())),
            Err(_) => Err(ScanError::Crashed(status)),
        }
    }
}

/// The scanner's exit code when it cannot scan at all: bad arguments, a format it was built
/// without, or no way to set up its output. Not the plugin's fault, so not cached.
const EXIT_UNSUPPORTED: u8 = 64;

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
        return Some(ExitCode::from(EXIT_UNSUPPORTED));
    };
    let Some(format) = kind
        .to_str()
        .and_then(kind_from_name)
        .and_then(|k| registry.format(&k))
    else {
        eprintln!("unsupported plugin format {kind:?}");
        return Some(ExitCode::from(EXIT_UNSUPPORTED));
    };
    // Plugins may print to stdout while loading. Point file descriptor 1 at stderr for the
    // scan and write the result to the real stdout, which is restored afterwards.
    let Some(redirect) = StdoutRedirect::to_stderr() else {
        return Some(ExitCode::from(EXIT_UNSUPPORTED));
    };
    let found = format.scan(Path::new(bundle));
    let json = serde_json::to_vec(&found).expect("descriptors serialize");
    let written = redirect.write_result(&json);
    drop(redirect);
    Some(if written.is_ok() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

/// Holds the real stdout while descriptor 1 points at stderr. Dropping it restores stdout.
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
        out.write_all(data)?;
        out.flush()
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
