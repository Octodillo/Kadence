//! One thread that watches every host process of the application.
//!
//! It sleeps in `poll` on a pidfd per host, so a host that exits is noticed at once and costs
//! nothing while it runs. Only while a host has an audio block to watch does the thread wake
//! on a coarse timer, to look for a cycle the host never finished: a hung plugin.
//!
//! A host runs in its own process group. Whoever ends it kills the whole group, so helper
//! processes the plugin started go with it, and the supervisor does the same when a host
//! exits by itself.

use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::process::{Child, Command, ExitStatus};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use daw_plugins::process::{in_own_group, kill_group};

use crate::shm::{Header, REQUEST, Shm};

/// How often stalled cycles are looked for. Hang limits are hundreds of milliseconds or more.
const TICK: Duration = Duration::from_millis(50);

/// How a host's end is reported, on the supervisor thread: keep both short, since every other
/// host waits meanwhile.
pub(crate) trait Reports: Send + Sync {
    /// The host exited though nobody asked it to.
    fn exited(&self, status: io::Result<ExitStatus>);
    /// A cycle stalled past its limit. Returns whether the host should be killed for it.
    fn hung(&self) -> bool;
}

/// How long a cycle may stall before the host counts as hung.
#[derive(Clone, Copy)]
pub(crate) struct Limits {
    pub hang: Duration,
    /// For a cycle that waits on the host's main thread while that is inside the plugin.
    pub call: Duration,
}

enum Process {
    Running(Child),
    /// Waited for; `None` if the wait itself failed.
    Exited(Option<ExitStatus>),
}

struct Watched {
    pid: u32,
    pidfd: OwnedFd,
    process: Mutex<Process>,
    exited: Condvar,
    /// An exit is expected and not reported.
    stopping: AtomicBool,
    /// The host loaded its plugin: from here on its exit is reported and `block` watched.
    armed: AtomicBool,
    /// The block the audio path uses, once there is one.
    block: Mutex<Option<Arc<Shm>>>,
    reports: Box<dyn Reports>,
    limits: Limits,
}

fn locked<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Watched {
    /// Kills the host and everything it started, unless it was already waited for.
    fn kill(&self) {
        // Under the lock the child is unreaped, so its process group id cannot be reused.
        if let Process::Running(child) = &*locked(&self.process) {
            kill_group(child);
        }
    }

    /// Waits for a host whose pidfd said it exited, and kills what it left running.
    fn reap(&self) -> io::Result<ExitStatus> {
        let mut process = locked(&self.process);
        let status = match &mut *process {
            Process::Running(child) => {
                kill_group(child);
                child.wait()
            }
            Process::Exited(status) => return status.ok_or_else(|| io::Error::other("gone")),
        };
        *process = Process::Exited(status.as_ref().ok().copied());
        drop(process);
        self.exited.notify_all();
        status
    }
}

/// A host process under supervision. Dropping it kills the host.
pub(crate) struct HostProcess(Arc<Watched>);

impl HostProcess {
    /// Starts `command` in its own process group and has the supervisor wait for it. Nothing
    /// is reported until [`arm`](Self::arm).
    pub(crate) fn spawn(
        command: &mut Command,
        reports: Box<dyn Reports>,
        limits: Limits,
    ) -> io::Result<Self> {
        let supervisor = Supervisor::get()?;
        let mut child = in_own_group(command).spawn()?;
        let pidfd = match pidfd_open(child.id()) {
            Ok(fd) => fd,
            Err(e) => {
                kill_group(&child);
                let _ = child.wait();
                return Err(e);
            }
        };
        let watched = Arc::new(Watched {
            pid: child.id(),
            pidfd,
            process: Mutex::new(Process::Running(child)),
            exited: Condvar::new(),
            stopping: AtomicBool::new(false),
            armed: AtomicBool::new(false),
            block: Mutex::new(None),
            reports,
            limits,
        });
        locked(&supervisor.new).push(watched.clone());
        supervisor.wake();
        Ok(Self(watched))
    }

    pub(crate) fn pid(&self) -> u32 {
        self.0.pid
    }

    /// Readable once the host has exited, for a caller that polls it next to something else.
    pub(crate) fn pidfd(&self) -> BorrowedFd<'_> {
        self.0.pidfd.as_fd()
    }

    /// The host is up: report its exit from now on, and watch `block` for hung cycles. An
    /// exit that came before this is reported too.
    pub(crate) fn arm(&self, block: Arc<Shm>) {
        self.watch(block);
        self.0.armed.store(true, Ordering::Release);
        Supervisor::wake_running();
    }

    /// Makes `block` the one watched for hung cycles.
    pub(crate) fn watch(&self, block: Arc<Shm>) {
        *locked(&self.0.block) = Some(block);
    }

    /// The host is about to be stopped: its exit is not reported.
    pub(crate) fn expect_exit(&self) {
        self.0.stopping.store(true, Ordering::Release);
    }

    /// Kills the host and every process it started.
    pub(crate) fn kill(&self) {
        self.0.kill();
    }

    /// Waits up to `timeout` for the host to exit. Returns its status once it has (`None` if
    /// that could not be read), or `Err` on a timeout.
    pub(crate) fn wait(&self, timeout: Duration) -> Result<Option<ExitStatus>, ()> {
        let (process, _) = self
            .0
            .exited
            .wait_timeout_while(locked(&self.0.process), timeout, |p| {
                matches!(p, Process::Running(_))
            })
            .unwrap_or_else(PoisonError::into_inner);
        match &*process {
            Process::Running(_) => Err(()),
            Process::Exited(status) => Ok(*status),
        }
    }

    /// Kills the host and waits until it is gone.
    pub(crate) fn kill_and_wait(&self) {
        self.expect_exit();
        self.kill();
        // SIGKILL cannot be refused, so this only runs out if the supervisor is stuck in a
        // report; the host is dead either way.
        if self.wait(Duration::from_secs(5)).is_err() {
            log::warn!("plugin host {}: not seen to exit after a kill", self.0.pid);
        }
    }
}

impl Drop for HostProcess {
    fn drop(&mut self) {
        self.expect_exit();
        self.kill();
        Supervisor::wake_running();
    }
}

fn pidfd_open(pid: u32) -> io::Result<OwnedFd> {
    // SAFETY: plain syscall; a non-negative result is a new descriptor that we own.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::pid_t, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: as above.
    Ok(unsafe { OwnedFd::from_raw_fd(fd as i32) })
}

struct Supervisor {
    /// Hosts started since the thread last looked.
    new: Mutex<Vec<Arc<Watched>>>,
    /// An eventfd that interrupts the thread's `poll`.
    wake: OwnedFd,
}

static SUPERVISOR: OnceLock<Result<Supervisor, String>> = OnceLock::new();

impl Supervisor {
    /// The supervisor, started on first use.
    fn get() -> io::Result<&'static Self> {
        SUPERVISOR
            .get_or_init(|| Self::start().map_err(|e| e.to_string()))
            .as_ref()
            .map_err(|e| io::Error::other(format!("cannot start the plugin supervisor: {e}")))
    }

    fn start() -> io::Result<Self> {
        // SAFETY: plain syscall; a non-negative result is a new descriptor that we own.
        let fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: as above.
        let wake = unsafe { OwnedFd::from_raw_fd(fd) };
        std::thread::Builder::new()
            .name("plugin-supervisor".into())
            .spawn(|| {
                let supervisor = Self::get().expect("started by the time the thread runs");
                supervisor.run();
            })?;
        Ok(Self {
            new: Mutex::default(),
            wake,
        })
    }

    fn wake(&self) {
        let one = 1u64.to_ne_bytes();
        // SAFETY: writes 8 bytes from a valid buffer to our eventfd. It fails only when the
        // counter is full, and then the thread is waking anyway.
        unsafe { libc::write(self.wake.as_raw_fd(), one.as_ptr().cast(), one.len()) };
    }

    /// Wakes the thread if there is one: without it there is nothing to tell.
    fn wake_running() {
        if let Some(Ok(supervisor)) = SUPERVISOR.get() {
            supervisor.wake();
        }
    }

    fn run(&self) -> ! {
        let mut hosts: Vec<Host> = Vec::new();
        let mut fds: Vec<libc::pollfd> = Vec::new();
        loop {
            hosts.extend(locked(&self.new).drain(..).map(Host::new));
            fds.clear();
            fds.push(pollfd(self.wake.as_raw_fd()));
            fds.extend(hosts.iter().map(|h| pollfd(h.watched.pidfd.as_raw_fd())));
            // Only watched cycles need the timer; exits wake the poll by themselves.
            let ticking = hosts.iter().any(Host::needs_tick);
            let timeout = if ticking { TICK.as_millis() as i32 } else { -1 };
            // SAFETY: `fds` is a valid array of that length. On an error (EINTR) the events
            // may be stale, which only costs a look at each host.
            unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout) };
            if fds[0].revents != 0 {
                let mut count = [0u8; 8];
                // SAFETY: reads 8 bytes into a valid buffer from our non-blocking eventfd.
                unsafe { libc::read(self.wake.as_raw_fd(), count.as_mut_ptr().cast(), 8) };
            }
            let mut exited = fds[1..].iter().map(|fd| fd.revents != 0);
            hosts.retain_mut(|host| host.look(exited.next().unwrap_or(false)));
        }
    }
}

fn pollfd(fd: i32) -> libc::pollfd {
    libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    }
}

/// What the supervisor thread keeps for one host.
struct Host {
    watched: Arc<Watched>,
    status: Option<io::Result<ExitStatus>>,
    stall: Stall,
}

impl Host {
    fn new(watched: Arc<Watched>) -> Self {
        Self {
            watched,
            status: None,
            stall: Stall::default(),
        }
    }

    fn needs_tick(&self) -> bool {
        self.status.is_none() && self.watched.armed.load(Ordering::Acquire)
    }

    /// Looks after the host once. Returns false when there is nothing left to do for it.
    fn look(&mut self, exited: bool) -> bool {
        let w = &*self.watched;
        if exited && self.status.is_none() {
            self.status = Some(w.reap());
        }
        if self.status.is_none() {
            if w.armed.load(Ordering::Acquire) {
                let block = locked(&w.block).clone();
                if let Some(block) = block
                    && self.stall.is_hang(&block, w.limits)
                    && guarded(|| w.reports.hung())
                {
                    w.kill();
                }
            }
            return true;
        }
        if w.stopping.load(Ordering::Acquire) {
            return false;
        }
        // An exit before the host was armed waits here: its owner arms it, or drops it.
        if !w.armed.load(Ordering::Acquire) {
            return true;
        }
        let status = self.status.take().expect("checked above");
        guarded(|| w.reports.exited(status));
        false
    }
}

/// Runs a report, which may call into the application: a panic there must not end the
/// supervision of every other host.
fn guarded<R: Default>(report: impl FnOnce() -> R) -> R {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(report)).unwrap_or_else(|_| {
        log::error!("plugin supervisor: a crash report panicked");
        R::default()
    })
}

/// Follows one host's cycles to tell a hung one.
#[derive(Default)]
struct Stall {
    block: Option<Arc<Shm>>,
    last_completed: u64,
    since: Option<Instant>,
}

impl Stall {
    /// Whether the cycle `block` waits on has stalled past its limit.
    fn is_hang(&mut self, block: &Arc<Shm>, limits: Limits) -> bool {
        let h = block.header();
        let completed = h.completed.load(Ordering::Relaxed);
        if !self.block.as_ref().is_some_and(|b| Arc::ptr_eq(b, block)) {
            *self = Self {
                block: Some(block.clone()),
                last_completed: completed,
                since: None,
            };
            return false;
        }
        let stalled =
            h.state.load(Ordering::Acquire) == REQUEST && completed == self.last_completed;
        match stall_limit(h, limits).filter(|_| stalled) {
            Some(limit) => self.since.get_or_insert_with(Instant::now).elapsed() >= limit,
            None => {
                self.since = None;
                self.last_completed = completed;
                false
            }
        }
    }
}

/// How long an audio cycle may stall before the host counts as hung, or `None` while the
/// stall is not a hang: the host's main thread is in a slow call (state, editor), or the cycle
/// waits for a main thread that is busy in the plugin's open editor, which a modal dialog
/// there keeps busy for as long as the user takes.
fn stall_limit(h: &Header, limits: Limits) -> Option<Duration> {
    if h.main_busy.load(Ordering::SeqCst) != 0 {
        return None;
    }
    if h.main_in_plugin.load(Ordering::SeqCst) == 0 {
        return Some(limits.hang);
    }
    if h.pumping_editor().is_some() {
        return None;
    }
    // The cycle may be waiting for the host's main thread, which is busy in the plugin (its
    // main-thread pump, say): that is as slow as a main-thread call may be.
    Some(limits.call.max(limits.hang))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicU32;

    use super::*;

    /// Counts what is reported.
    #[derive(Default)]
    struct Seen {
        hung: AtomicU32,
        exited: AtomicU32,
    }

    impl Reports for Arc<Seen> {
        fn exited(&self, _status: io::Result<ExitStatus>) {
            self.exited.fetch_add(1, Ordering::SeqCst);
        }
        fn hung(&self) -> bool {
            self.hung.fetch_add(1, Ordering::SeqCst) == 0
        }
    }

    fn limits(hang: Duration, call: Duration) -> Limits {
        Limits { hang, call }
    }

    fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
        let start = Instant::now();
        while !done() {
            assert!(start.elapsed() < Duration::from_secs(30), "{what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn sleeper(seen: &Arc<Seen>, limits: Limits) -> HostProcess {
        HostProcess::spawn(
            Command::new("sleep").arg("60"),
            Box::new(seen.clone()),
            limits,
        )
        .unwrap()
    }

    #[test]
    fn a_stall_while_the_host_main_thread_is_in_the_plugin_gets_the_call_timeout() {
        let seen = Arc::new(Seen::default());
        let host = sleeper(
            &seen,
            limits(Duration::from_millis(20), Duration::from_secs(3600)),
        );
        let shm = Shm::scratch();
        // A cycle is waiting on the plugin lock, which the host's main thread holds.
        shm.header().state.store(REQUEST, Ordering::Release);
        shm.header().main_in_plugin.store(1, Ordering::SeqCst);
        host.arm(shm.clone());
        // Many hang timeouts pass without a kill.
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(seen.hung.load(Ordering::SeqCst), 0);
        // Once the main thread is out of the plugin, the stall is a hang: the host is killed,
        // and its exit reported.
        shm.header().main_in_plugin.store(0, Ordering::SeqCst);
        wait_for("hang not noticed", || {
            seen.exited.load(Ordering::SeqCst) == 1
        });
        assert_eq!(seen.hung.load(Ordering::SeqCst), 1);
        assert!(host.wait(Duration::ZERO).is_ok());
    }

    #[test]
    fn an_exit_is_reported_once_the_host_is_armed_and_not_when_it_was_expected() {
        let seen = Arc::new(Seen::default());
        let quiet = limits(Duration::from_secs(60), Duration::from_secs(60));
        // Exits before it is armed: reported when it is.
        let early =
            HostProcess::spawn(&mut Command::new("true"), Box::new(seen.clone()), quiet).unwrap();
        assert!(
            early
                .wait(Duration::from_secs(30))
                .unwrap()
                .unwrap()
                .success()
        );
        assert_eq!(seen.exited.load(Ordering::SeqCst), 0);
        early.arm(Shm::scratch());
        wait_for("early exit not reported", || {
            seen.exited.load(Ordering::SeqCst) == 1
        });

        // Killed behind our back: reported.
        let host = sleeper(&seen, quiet);
        host.arm(Shm::scratch());
        host.kill();
        wait_for("exit not reported", || {
            seen.exited.load(Ordering::SeqCst) == 2
        });

        // Stopped on purpose: not reported.
        let host = sleeper(&seen, quiet);
        host.arm(Shm::scratch());
        host.kill_and_wait();
        drop(host);
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(seen.exited.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn killing_a_host_kills_the_helpers_it_started() {
        let seen = Arc::new(Seen::default());
        let quiet = limits(Duration::from_secs(60), Duration::from_secs(60));
        // A shell that starts a helper and prints its pid.
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("helper.pid");
        let script = format!("sleep 60 & echo $! > {}; wait", pid_file.display());
        let host = HostProcess::spawn(
            Command::new("sh").arg("-c").arg(script),
            Box::new(seen.clone()),
            quiet,
        )
        .unwrap();
        let mut helper = None;
        wait_for("the helper to start", || {
            helper = std::fs::read_to_string(&pid_file)
                .ok()
                .and_then(|s| s.trim().parse::<libc::pid_t>().ok());
            helper.is_some()
        });
        let helper = helper.unwrap();
        host.kill_and_wait();
        // SAFETY: signal 0 only asks whether the process exists. A zombie still does, until
        // init reaps it.
        wait_for("the helper to die", || {
            let stat = std::fs::read_to_string(format!("/proc/{helper}/stat")).unwrap_or_default();
            let zombie = stat
                .rsplit(')')
                .next()
                .is_some_and(|s| s.trim_start().starts_with('Z'));
            zombie || unsafe { libc::kill(helper, 0) } != 0
        });
    }

    #[test]
    fn a_stall_on_a_main_thread_busy_in_the_editor_is_not_a_hang() {
        let shm = Shm::scratch();
        let h = shm.header();
        let (hang, call) = (Duration::from_secs(1), Duration::from_secs(10));
        let l = limits(hang, call);
        assert_eq!(stall_limit(h, l), Some(hang));
        // The cycle waits for the main thread, which is in a pump.
        h.main_in_plugin.store(1, Ordering::SeqCst);
        h.enter_pump();
        assert_eq!(stall_limit(h, l), Some(call));
        // With the editor open, the user may have a dialog of the plugin's up.
        h.editor_open.store(1, Ordering::Release);
        assert_eq!(stall_limit(h, l), None);
        // Out of the pump, it counts again.
        h.leave_pump();
        assert_eq!(stall_limit(h, l), Some(call));
        // A split plugin's audio never waits for the main thread, so its stall is a hang
        // whatever the main thread does.
        h.enter_pump();
        h.main_in_plugin.store(0, Ordering::SeqCst);
        assert_eq!(stall_limit(h, l), Some(hang));
        h.main_busy.store(1, Ordering::SeqCst);
        assert_eq!(stall_limit(h, l), None);
    }
}
