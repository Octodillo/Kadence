//! The application side of a sandbox: [`SandboxedInstance`] is a [`PluginInstance`] whose plugin
//! runs in a host process.

use std::ffi::OsString;
use std::fmt;
use std::io;
use std::os::fd::AsFd;
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use daw_core::event::{Event, EventKind, EventList, ParamId};
use daw_core::node::{Node, PortLayout, ProcessContext};
use daw_core::plugin::{ParamInfo, PluginDescriptor, PluginError, PluginInstance};
use daw_plugins::Scanner;

use crate::child::HOST_ARG;
use crate::protocol::{
    FrameReader, InstanceInfo, Reply, Request, Response, blob_fits, write_frame,
};
use crate::shm::{
    self, DONE, EVENT_CAPACITY, FLAG_RESET, Layout, REQUEST, Shm, decode_event, encode_event,
    futex_wait, futex_wake, shm_dir,
};

/// How to start a host process: `<program> <args>... --plugin-host <socket>`.
#[derive(Clone, Debug)]
pub struct HostLauncher {
    program: PathBuf,
    args: Vec<OsString>,
}

impl HostLauncher {
    /// Runs `program` with `args` before the host arguments.
    pub fn new(
        program: impl Into<PathBuf>,
        args: impl IntoIterator<Item = impl Into<OsString>>,
    ) -> Self {
        Self {
            program: program.into(),
            args: args.into_iter().map(Into::into).collect(),
        }
    }

    /// Runs the current executable, for applications that forward `--plugin-host` to
    /// [`run_host_child`](crate::run_host_child).
    pub fn current_exe() -> io::Result<Self> {
        Ok(Self::new(std::env::current_exe()?, Vec::<OsString>::new()))
    }

    /// Runs the `daw-plugin-host` binary installed next to the current executable.
    pub fn sibling() -> io::Result<Self> {
        let exe = std::env::current_exe()?;
        let dir = exe.parent().unwrap_or(Path::new("."));
        Ok(Self::new(
            dir.join("daw-plugin-host"),
            Vec::<OsString>::new(),
        ))
    }

    /// A scanner that runs the same program, which also answers `--scan`.
    pub fn scanner(&self) -> Scanner {
        Scanner::new(self.program.clone(), self.args.clone())
    }
}

/// Called on a watchdog thread when a sandboxed plugin crashes or hangs.
pub type CrashHook = Arc<dyn Fn(SandboxMonitor) + Send + Sync>;

/// Settings for sandboxed instances.
#[derive(Clone)]
pub struct SandboxConfig {
    pub launcher: HostLauncher,
    /// How long starting the host and loading the plugin, or saving or loading its state, may
    /// take. Default 30 s.
    pub load_timeout: Duration,
    /// How long other main-thread calls (params, editor, prepare) may take before the host is
    /// treated as hung, and how long an audio cycle may wait while the host's main thread is in
    /// the plugin. Default 10 s.
    pub call_timeout: Duration,
    /// How long one audio cycle may stay unfinished before the host is treated as hung and
    /// killed. Default 2 s.
    pub hang_timeout: Duration,
    /// How long `process` waits for the host before giving up on a cycle and outputting
    /// silence. `None` (the default) waits at most one buffer's duration, since any longer is
    /// an xrun anyway.
    pub process_timeout: Option<Duration>,
    /// `SCHED_FIFO` priority for the host's audio thread; `None` leaves it at normal
    /// priority. Default 70, below JACK's own threads. Failure to get it is logged by the host.
    pub rt_priority: Option<i32>,
    /// Told when a plugin crashes or hangs, so the UI can offer to reload it.
    pub on_crash: Option<CrashHook>,
}

impl SandboxConfig {
    pub fn new(launcher: HostLauncher) -> Self {
        Self {
            launcher,
            load_timeout: Duration::from_secs(30),
            call_timeout: Duration::from_secs(10),
            hang_timeout: Duration::from_secs(2),
            process_timeout: None,
            rt_priority: Some(70),
            on_crash: None,
        }
    }
}

impl fmt::Debug for SandboxConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SandboxConfig")
            .field("launcher", &self.launcher)
            .field("load_timeout", &self.load_timeout)
            .field("call_timeout", &self.call_timeout)
            .field("hang_timeout", &self.hang_timeout)
            .field("process_timeout", &self.process_timeout)
            .field("rt_priority", &self.rt_priority)
            .field("on_crash", &self.on_crash.is_some())
            .finish()
    }
}

/// Why a sandboxed plugin stopped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CrashReason {
    /// The host process exited or was killed.
    Exited(String),
    /// An audio cycle did not finish within the hang timeout, so the host was killed.
    Hung,
    /// A main-thread call failed or timed out, so the host was killed.
    Lost(String),
}

impl fmt::Display for CrashReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Exited(status) => write!(f, "the plugin crashed ({status})"),
            Self::Hung => write!(f, "the plugin stopped responding"),
            Self::Lost(why) => write!(f, "lost contact with the plugin: {why}"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SandboxStatus {
    Running,
    Crashed(CrashReason),
}

/// State shared between an instance, its watchdog and any [`SandboxMonitor`]s.
struct Shared {
    descriptor: PluginDescriptor,
    /// Read on the audio thread; the details are in `reason`.
    crashed: AtomicBool,
    reason: Mutex<Option<CrashReason>>,
    /// Which host process is current; bumped on every (re)start.
    generation: AtomicU64,
    reload_requested: AtomicBool,
    pid: AtomicU32,
    late_cycles: AtomicU64,
    on_crash: Option<CrashHook>,
    /// The last generation the crash hook was called for.
    notified: AtomicU64,
}

impl Shared {
    fn reason(&self) -> MutexGuard<'_, Option<CrashReason>> {
        self.reason.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Marks host `generation` as crashed, unless a newer host has replaced it or it already
    /// crashed. Returns whether this call marked it. Whoever marks a crash also kills the
    /// host, so the watchdog sees it exit and calls the hook.
    fn crash(&self, generation: u64, reason: CrashReason) -> bool {
        let mut current = self.reason();
        if self.generation.load(Ordering::Acquire) != generation || current.is_some() {
            return false;
        }
        *current = Some(reason);
        self.crashed.store(true, Ordering::Release);
        true
    }

    /// Calls the crash hook once for `generation`, if it crashed and is still current. Only
    /// watchdog threads call this, so the hook never runs on a thread that holds the instance.
    fn notify(self: &Arc<Self>, generation: u64) {
        let current = self.generation.load(Ordering::Acquire) == generation;
        if current
            && self.crashed.load(Ordering::Acquire)
            && self.notified.swap(generation, Ordering::AcqRel) != generation
            && let Some(hook) = &self.on_crash
        {
            hook(SandboxMonitor(self.clone()));
        }
    }
}

/// A handle on a sandboxed instance's health, usable from any thread, such as the UI's.
#[derive(Clone)]
pub struct SandboxMonitor(Arc<Shared>);

impl SandboxMonitor {
    pub fn descriptor(&self) -> &PluginDescriptor {
        &self.0.descriptor
    }

    pub fn status(&self) -> SandboxStatus {
        match self.0.reason().clone() {
            Some(reason) => SandboxStatus::Crashed(reason),
            None => SandboxStatus::Running,
        }
    }

    pub fn is_crashed(&self) -> bool {
        self.0.crashed.load(Ordering::Acquire)
    }

    /// Asks the instance to restart its host and restore its state. It happens on the next
    /// [`PluginInstance::on_main_thread`] call.
    pub fn request_reload(&self) {
        self.0.reload_requested.store(true, Ordering::Release);
    }

    /// The current host process id.
    pub fn pid(&self) -> u32 {
        self.0.pid.load(Ordering::Relaxed)
    }

    /// How many times the host has been started (1 before any reload).
    pub fn generation(&self) -> u64 {
        self.0.generation.load(Ordering::Acquire)
    }

    /// Audio cycles that got silence because the host was not done in time.
    pub fn late_cycles(&self) -> u64 {
        self.0.late_cycles.load(Ordering::Relaxed)
    }
}

impl fmt::Debug for SandboxMonitor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SandboxMonitor")
            .field("plugin", &self.0.descriptor.id)
            .field("status", &self.status())
            .field("pid", &self.pid())
            .finish()
    }
}

/// One running host process.
struct Link {
    generation: u64,
    child: Arc<Mutex<Child>>,
    control: UnixStream,
    /// The block the audio path uses.
    shm: Arc<Shm>,
    /// The same block, for the watchdog; swapped when `prepare` needs a bigger one.
    watched: Arc<Mutex<Arc<Shm>>>,
    stop: Arc<AtomicBool>,
    watchdog: Option<JoinHandle<()>>,
}

fn locked<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Removes a file when dropped.
struct RemoveOnDrop(PathBuf);

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        if std::fs::remove_file(&self.0).is_err() {
            let _ = std::fs::remove_dir(&self.0);
        }
    }
}

fn socket_dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|d| d.is_dir())
        .unwrap_or_else(std::env::temp_dir)
}

/// Creates a new directory in `parent` that only we can enter. Its name is unpredictable, and a
/// name already taken (by another user in a shared `/tmp`, say) is skipped.
fn create_socket_dir(parent: &Path) -> io::Result<PathBuf> {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    for _ in 0..64 {
        let mut random = [0u8; 8];
        // SAFETY: writes at most `random.len()` bytes into `random`.
        let got = unsafe { libc::getrandom(random.as_mut_ptr().cast(), random.len(), 0) };
        let random = if got == random.len() as isize {
            u64::from_ne_bytes(random)
        } else {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos() as u64)
        };
        let dir = parent.join(format!(
            "kadence-plugin-{}-{}-{random:016x}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "no free name for the host socket directory",
    ))
}

fn kill(child: &Mutex<Child>) {
    let mut child = locked(child);
    let _ = child.kill();
    let _ = child.wait();
}

impl Link {
    /// Starts a host, loads the plugin and maps its audio block.
    fn start(
        config: &SandboxConfig,
        shared: &Arc<Shared>,
        generation: u64,
        sample_rate: f64,
        max_frames: u32,
    ) -> Result<(Self, InstanceInfo, u32), PluginError> {
        let load = |e: String| PluginError::Load(e);

        // The socket lives in a directory only we can enter, so no other user can connect to
        // it before our host does.
        let dir = create_socket_dir(&socket_dir())
            .map_err(|e| load(format!("cannot create the host socket directory: {e}")))?;
        let dir = RemoveOnDrop(dir);
        let socket = RemoveOnDrop(dir.0.join("host.sock"));
        let listener = UnixListener::bind(&socket.0)
            .map_err(|e| load(format!("cannot create the host socket: {e}")))?;
        listener
            .set_nonblocking(true)
            .map_err(|e| load(e.to_string()))?;

        // Plugins print to stdout; send that to our stderr so it is seen but never mixed with
        // anything we read.
        let stdout = io::stderr()
            .as_fd()
            .try_clone_to_owned()
            .map(Stdio::from)
            .unwrap_or_else(|_| Stdio::null());
        let child = Command::new(&config.launcher.program)
            .args(&config.launcher.args)
            .arg(HOST_ARG)
            .arg(&socket.0)
            .stdin(Stdio::null())
            .stdout(stdout)
            .spawn()
            .map_err(|e| {
                load(format!(
                    "cannot start the plugin host {:?}: {e}",
                    config.launcher.program
                ))
            })?;
        let pid = child.id();
        let child = Arc::new(Mutex::new(child));

        let started = (|| {
            let deadline = Instant::now() + config.load_timeout;
            let control = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        if let Ok(Some(status)) = locked(&child).try_wait() {
                            return Err(load(format!(
                                "the plugin host exited at start ({status})"
                            )));
                        }
                        if Instant::now() >= deadline {
                            return Err(load("the plugin host did not start in time".into()));
                        }
                        std::thread::sleep(Duration::from_millis(2));
                    }
                    Err(e) => return Err(load(format!("plugin host connection failed: {e}"))),
                }
            };
            drop(socket);
            drop(dir);
            control
                .set_nonblocking(false)
                .map_err(|e| load(e.to_string()))?;

            let (response, _) = call(
                &control,
                &Request::Instantiate {
                    descriptor: shared.descriptor.clone(),
                    sample_rate,
                    max_frames,
                    rt_priority: config.rt_priority,
                },
                &[],
                config.load_timeout,
            )
            .map_err(|e| load(format!("plugin host: {e}")))?;
            let info = match response.reply {
                Reply::Instantiated(info) => info,
                Reply::Err(e) => return Err(e.into()),
                other => return Err(load(format!("plugin host: unexpected reply {other:?}"))),
            };

            let layout = Layout {
                audio_in: u32::from(info.ports.audio_in),
                audio_out: u32::from(info.ports.audio_out),
                max_frames: max_frames.max(1),
                event_capacity: EVENT_CAPACITY,
            };
            // Instantiating prepared the plugin with these settings already.
            let (shm, latency, _) =
                map_block(&control, layout, sample_rate, max_frames, false, config)?;
            Ok((control, info, shm, latency))
        })();
        let (control, info, shm, latency) = match started {
            Ok(v) => v,
            Err(e) => {
                kill(&child);
                return Err(e);
            }
        };

        let shm = Arc::new(shm);
        let watched = Arc::new(Mutex::new(shm.clone()));
        let stop = Arc::new(AtomicBool::new(false));
        shared.pid.store(pid, Ordering::Relaxed);
        let spawned = {
            let (child, watched, stop, shared) =
                (child.clone(), watched.clone(), stop.clone(), shared.clone());
            let (hang_timeout, call_timeout) = (config.hang_timeout, config.call_timeout);
            std::thread::Builder::new()
                .name("plugin-watchdog".into())
                .spawn(move || {
                    watchdog(
                        &child,
                        &watched,
                        &stop,
                        &shared,
                        generation,
                        hang_timeout,
                        call_timeout,
                    )
                })
        };
        let watchdog = match spawned {
            Ok(handle) => handle,
            Err(e) => {
                kill(&child);
                return Err(load(format!("cannot start the plugin watchdog: {e}")));
            }
        };
        Ok((
            Self {
                generation,
                child,
                control,
                shm,
                watched,
                stop,
                watchdog: Some(watchdog),
            },
            info,
            latency,
        ))
    }

    /// Stops the host: politely if `graceful`, then by force.
    fn close(mut self, graceful: bool, timeout: Duration) {
        self.stop.store(true, Ordering::Release);
        if graceful {
            let _ = call(&self.control, &Request::Shutdown, &[], timeout);
            let deadline = Instant::now() + timeout;
            while Instant::now() < deadline {
                if matches!(locked(&self.child).try_wait(), Ok(Some(_))) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
        }
        kill(&self.child);
        if let Some(watchdog) = self.watchdog.take() {
            let _ = watchdog.join();
        }
    }
}

/// Creates a block for `layout`, has the host map it (and prepare the plugin, if `prepare`),
/// then removes its file. Returns the block, the plugin's latency and its ports.
fn map_block(
    control: &UnixStream,
    layout: Layout,
    sample_rate: f64,
    max_frames: u32,
    prepare: bool,
    config: &SandboxConfig,
) -> Result<(Shm, u32, PortLayout), PluginError> {
    let (shm, path) = Shm::create(&shm_dir(), layout)
        .map_err(|e| PluginError::Load(format!("cannot create the audio block: {e}")))?;
    let path = RemoveOnDrop(path);
    let (response, _) = call(
        control,
        &Request::Prepare {
            sample_rate,
            max_frames,
            shm: Some(path.0.clone()),
            prepare,
        },
        &[],
        config.call_timeout,
    )
    .map_err(|e| PluginError::Load(format!("plugin host: {e}")))?;
    match response.reply {
        Reply::Prepared(ports) => Ok((shm, response.latency, (&ports).into())),
        Reply::Err(e) => Err(e.into()),
        other => Err(PluginError::Load(format!(
            "plugin host: unexpected reply {other:?}"
        ))),
    }
}

/// One request and its response, all within `timeout`, however the host paces its bytes.
fn call(
    control: &UnixStream,
    request: &Request,
    blob: &[u8],
    timeout: Duration,
) -> io::Result<(Response, Vec<u8>)> {
    let mut stream = Deadline {
        stream: control,
        at: Instant::now() + timeout.max(Duration::from_millis(1)),
    };
    write_frame(&mut stream, request, blob)?;
    // The reader grows its buffer as bytes arrive, rather than trusting the announced length.
    let mut reader = FrameReader::default();
    loop {
        if let Some(frame) = reader.poll(&mut stream)? {
            return Ok(frame);
        }
        stream.remaining()?;
    }
}

/// A stream whose reads and writes all end by one deadline: each syscall gets the time left.
struct Deadline<'a> {
    stream: &'a UnixStream,
    at: Instant,
}

impl Deadline<'_> {
    fn remaining(&self) -> io::Result<Duration> {
        let left = self.at.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "the plugin host did not answer in time",
            ));
        }
        Ok(left)
    }
}

impl io::Read for Deadline<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.stream.set_read_timeout(Some(self.remaining()?))?;
        (&mut &*self.stream).read(buf)
    }
}

impl io::Write for Deadline<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.stream.set_write_timeout(Some(self.remaining()?))?;
        (&mut &*self.stream).write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn watchdog(
    child: &Mutex<Child>,
    watched: &Mutex<Arc<Shm>>,
    stop: &AtomicBool,
    shared: &Arc<Shared>,
    generation: u64,
    hang_timeout: Duration,
    call_timeout: Duration,
) {
    let mut block: Option<Arc<Shm>> = None;
    let mut last_completed = 0;
    let mut stalled_since: Option<Instant> = None;
    loop {
        if stop.load(Ordering::Acquire) {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
        match locked(child).try_wait() {
            Ok(None) => {}
            Ok(Some(status)) => {
                if !stop.load(Ordering::Acquire) {
                    shared.crash(generation, CrashReason::Exited(status.to_string()));
                    shared.notify(generation);
                }
                return;
            }
            Err(e) => {
                shared.crash(generation, CrashReason::Lost(e.to_string()));
                shared.notify(generation);
                return;
            }
        }

        let current = locked(watched).clone();
        if !block.as_ref().is_some_and(|b| Arc::ptr_eq(b, &current)) {
            last_completed = current.header().completed.load(Ordering::Relaxed);
            stalled_since = None;
            block = Some(current);
            continue;
        }
        let h = block.as_ref().expect("set above").header();
        let completed = h.completed.load(Ordering::Relaxed);
        let stalled = h.state.load(Ordering::Acquire) == REQUEST
            && completed == last_completed
            && h.main_busy.load(Ordering::SeqCst) == 0;
        if stalled {
            let since = *stalled_since.get_or_insert_with(Instant::now);
            // The cycle may be waiting for the host's main thread, which is busy in the plugin
            // (its main-thread pump, say): that is as slow as a main-thread call may be.
            let limit = if h.main_in_plugin.load(Ordering::SeqCst) != 0 {
                call_timeout.max(hang_timeout)
            } else {
                hang_timeout
            };
            if since.elapsed() >= limit && shared.crash(generation, CrashReason::Hung) {
                let _ = locked(child).kill();
            }
        } else {
            stalled_since = None;
            last_completed = completed;
        }
    }
}

/// The last value the application or the plugin set for one automatable parameter, so a
/// reload can restore it on top of the last saved state.
#[derive(Clone, Copy)]
struct TrackedParam {
    id: ParamId,
    value: f64,
    /// Changed since the last saved or loaded state.
    dirty: bool,
}

/// What to track for `params`: the automatable ones, sorted by id, taking value and dirty flag
/// from `old` (sorted by id too) where it has them.
fn tracked_params(params: &[ParamInfo], old: &[TrackedParam]) -> Vec<TrackedParam> {
    let mut tracked: Vec<_> = params
        .iter()
        .filter(|p| p.automatable)
        .map(|p| TrackedParam {
            id: p.id,
            value: p.default,
            dirty: false,
        })
        .collect();
    tracked.sort_by_key(|t| t.id);
    tracked.dedup_by_key(|t| t.id);
    for t in &mut tracked {
        if let Ok(i) = old.binary_search_by_key(&t.id, |o| o.id) {
            *t = old[i];
        }
    }
    tracked
}

/// A [`PluginInstance`] whose plugin runs in its own host process.
///
/// Audio goes through shared memory; `process` hands each cycle to the host and waits for it
/// with a deadline, without allocating or locking. If the host crashes or hangs, `process`
/// outputs silence, the [`SandboxMonitor`] reports [`SandboxStatus::Crashed`] and the
/// configured crash hook is called. [`SandboxedInstance::reload`] (or
/// [`SandboxMonitor::request_reload`]) starts a new host and restores the last saved or loaded
/// state plus any parameter changes made since.
pub struct SandboxedInstance {
    config: SandboxConfig,
    shared: Arc<Shared>,
    link: Option<Link>,
    ports: PortLayout,
    params: Vec<ParamInfo>,
    has_editor: bool,
    editor_open: bool,
    latency: u32,
    /// The host reported that the plugin wants to be prepared again; see
    /// [`PluginInstance::take_layout_changed`].
    layout_changed: bool,
    /// The settings the plugin in the host is prepared with, while nothing has asked for
    /// another prepare since. A prepare with them has nothing to do.
    prepared_with: Option<(f64, u32)>,
    sample_rate: f64,
    max_frames: u32,
    /// The last state saved from or loaded into the plugin.
    state: Option<Vec<u8>>,
    rt: Rt,
}

impl SandboxedInstance {
    /// Starts a host process and loads `descriptor` in it.
    pub fn new(
        descriptor: PluginDescriptor,
        config: SandboxConfig,
        sample_rate: f64,
        max_frames: u32,
    ) -> Result<Self, PluginError> {
        let shared = Arc::new(Shared {
            descriptor,
            crashed: AtomicBool::new(false),
            reason: Mutex::new(None),
            generation: AtomicU64::new(1),
            reload_requested: AtomicBool::new(false),
            pid: AtomicU32::new(0),
            late_cycles: AtomicU64::new(0),
            on_crash: config.on_crash.clone(),
            notified: AtomicU64::new(0),
        });
        let (link, info, latency) = Link::start(&config, &shared, 1, sample_rate, max_frames)?;
        let mut instance = Self {
            config,
            shared,
            link: Some(link),
            ports: PortLayout::default(),
            params: Vec::new(),
            has_editor: false,
            editor_open: false,
            latency,
            layout_changed: false,
            // Instantiating prepared it.
            prepared_with: Some((sample_rate, max_frames.max(1))),
            sample_rate,
            max_frames,
            state: None,
            rt: Rt::new(),
        };
        instance.adopt(&info);
        Ok(instance)
    }

    pub fn monitor(&self) -> SandboxMonitor {
        SandboxMonitor(self.shared.clone())
    }

    /// Restarts the host and restores the plugin: the last saved or loaded state, then the
    /// parameter changes made since, then the editor if it was open. Works whether or not the
    /// old host crashed.
    pub fn reload(&mut self) -> Result<(), PluginError> {
        self.shared.reload_requested.store(false, Ordering::Release);
        // A new generation, so the old host's watchdog can no longer report, and a clean slate
        // for the new one's. Nothing processes while this runs: it has `&mut self`.
        let generation = {
            let mut reason = self.shared.reason();
            *reason = None;
            self.shared.crashed.store(false, Ordering::Release);
            self.shared.generation.fetch_add(1, Ordering::AcqRel) + 1
        };
        if let Some(link) = self.link.take() {
            link.close(false, Duration::ZERO);
        }
        let (link, info, latency) = match Link::start(
            &self.config,
            &self.shared,
            generation,
            self.sample_rate,
            self.max_frames,
        ) {
            Ok(v) => v,
            Err(e) => {
                *self.shared.reason() = Some(CrashReason::Lost(e.to_string()));
                self.shared.crashed.store(true, Ordering::Release);
                // No watchdog runs for this generation, so tell the hook from a thread of our
                // own: never this one, which holds the instance.
                let shared = self.shared.clone();
                let _ = std::thread::Builder::new()
                    .name("plugin-watchdog".into())
                    .spawn(move || shared.notify(generation));
                return Err(e);
            }
        };
        self.link = Some(link);
        let (old_ports, old_latency) = (self.ports.clone(), self.latency);
        self.latency = latency;
        let kept = std::mem::take(&mut self.rt.tracked);
        self.adopt(&info);
        // Events carried for the old host are kept; a late cycle's outputs are gone with it.
        self.rt.late_pending = false;
        let restored = match self.state.clone() {
            Some(state) => self
                .control_with(Request::LoadState, &state, self.config.load_timeout)
                .map(|(_, _, latency)| {
                    self.latency = latency;
                    // The state can bring parameters the freshly loaded plugin did not have.
                    self.refresh_params();
                }),
            None => Ok(()),
        };
        for t in kept.into_iter().filter(|t| t.dirty) {
            if let Ok(i) = self.rt.tracked.binary_search_by_key(&t.id, |p| p.id) {
                self.rt.tracked[i] = t;
            }
        }
        self.rt.resend = self.rt.tracked.iter().any(|t| t.dirty).then_some(0);
        if self.editor_open {
            self.editor_open = false;
            let _ = self.open_editor();
        }
        // The new host's plugin may not match what the engine last prepared. Ports and latency
        // stay as the engine has them until it prepares again; `process` copes with a block
        // of other channel counts meanwhile.
        // The new host prepared its plugin with the current settings.
        self.prepared_with = Some((self.sample_rate, self.max_frames));
        if self.ports != old_ports || self.latency != old_latency {
            self.ports = old_ports;
            self.latency = old_latency;
            self.layout_changed = true;
            self.prepared_with = None;
        }
        restored
    }

    fn adopt(&mut self, info: &InstanceInfo) {
        self.ports = (&info.ports).into();
        self.params = info.params.iter().map(ParamInfo::from).collect();
        self.has_editor = info.has_editor;
        self.rt.tracked = tracked_params(&self.params, &[]);
    }

    /// Asks the host for its parameter list, which loading state can change, and tracks that
    /// list, keeping what was tracked for the parameters still in it.
    fn refresh_params(&mut self) {
        if let Ok((Reply::Params(params), _, _)) = self.control(Request::Params, &[]) {
            self.params = params.iter().map(ParamInfo::from).collect();
            self.rt.tracked = tracked_params(&self.params, &self.rt.tracked);
            self.rt.resend = self.rt.tracked.iter().any(|t| t.dirty).then_some(0);
        }
    }

    /// Prepares the plugin in the host with `sample_rate` and `max_frames`, with a new audio
    /// block when the plugin needs more frames or other channel counts than the current one.
    fn prepare_host(&mut self) -> Result<(), PluginError> {
        let link = self.link.as_ref().expect("checked by the caller");
        let block = link.shm.layout();
        let (shm, latency, ports) = if self.max_frames > block.max_frames {
            let layout = Layout {
                max_frames: self.max_frames,
                ..block
            };
            let (shm, latency, ports) = map_block(
                &link.control,
                layout,
                self.sample_rate,
                self.max_frames,
                true,
                &self.config,
            )?;
            (Some(shm), latency, ports)
        } else {
            let request = Request::Prepare {
                sample_rate: self.sample_rate,
                max_frames: self.max_frames,
                shm: None,
                prepare: true,
            };
            match self.control(request, &[])? {
                (Reply::Prepared(ports), _, latency) => (None, latency, (&ports).into()),
                (other, _, _) => {
                    return Err(PluginError::Load(format!(
                        "plugin host: unexpected reply {other:?}"
                    )));
                }
            }
        };
        if let Some(shm) = shm {
            self.install_block(shm);
        }
        self.latency = latency;
        self.ports = ports;

        // A new layout can bring other channel counts, which the block must carry.
        let link = self.link.as_ref().expect("still there");
        let block = link.shm.layout();
        let (audio_in, audio_out) = (
            u32::from(self.ports.audio_in),
            u32::from(self.ports.audio_out),
        );
        if (audio_in, audio_out) != (block.audio_in, block.audio_out) {
            let layout = Layout {
                audio_in,
                audio_out,
                ..block
            };
            // The plugin is prepared already; this only swaps the block.
            let (shm, latency, ports) = map_block(
                &link.control,
                layout,
                self.sample_rate,
                self.max_frames,
                false,
                &self.config,
            )?;
            self.install_block(shm);
            self.latency = latency;
            self.ports = ports;
        }
        Ok(())
    }

    /// Makes `shm` the block that `process` and the watchdog use.
    fn install_block(&mut self, shm: Shm) {
        if let Some(link) = self.link.as_mut() {
            let shm = Arc::new(shm);
            *locked(&link.watched) = shm.clone();
            link.shm = shm;
        }
    }

    fn running(&self) -> Option<&Link> {
        self.link
            .as_ref()
            .filter(|_| !self.shared.crashed.load(Ordering::Acquire))
    }

    /// Sends a main-thread request. A transport failure or timeout kills the host and marks it
    /// lost; a plugin error is returned as is.
    /// Returns the reply, its blob and the plugin's latency after the call.
    fn control(&self, request: Request, blob: &[u8]) -> Result<(Reply, Vec<u8>, u32), PluginError> {
        self.control_with(request, blob, self.config.call_timeout)
    }

    /// [`Self::control`] with its own timeout.
    fn control_with(
        &self,
        request: Request,
        blob: &[u8],
        timeout: Duration,
    ) -> Result<(Reply, Vec<u8>, u32), PluginError> {
        let Some(link) = self.running() else {
            return Err(PluginError::Load(self.not_running()));
        };
        match call(&link.control, &request, blob, timeout) {
            Ok((response, blob)) => match response.reply {
                Reply::Err(e) => Err(e.into()),
                reply => Ok((reply, blob, response.latency)),
            },
            Err(e) => {
                let reason = CrashReason::Lost(e.to_string());
                self.shared.crash(link.generation, reason.clone());
                kill(&link.child);
                Err(PluginError::Load(reason.to_string()))
            }
        }
    }

    fn not_running(&self) -> String {
        match self.shared.reason().clone() {
            Some(reason) => reason.to_string(),
            None => "the plugin host is not running".into(),
        }
    }

    fn mark_clean(&mut self) {
        for t in &mut self.rt.tracked {
            t.dirty = false;
        }
        self.rt.resend = None;
    }

    /// After a save, what the saved state holds is clean. Values still waiting to be re-sent
    /// after a reload, or carried to the next cycle, have not reached the plugin and so are not
    /// in that state: they stay dirty, and still go out.
    fn mark_saved(&mut self) {
        let pending = self.rt.resend.unwrap_or(self.rt.tracked.len());
        let carry = &self.rt.carry;
        for (i, t) in self.rt.tracked.iter_mut().enumerate() {
            let carried = carry
                .iter()
                .any(|e| matches!(e.kind, EventKind::ParamValue { param, .. } if param == t.id));
            if i < pending && !carried {
                t.dirty = false;
            }
        }
    }

    fn silence(ctx: &mut ProcessContext, from: usize) {
        for out in ctx.audio_out.iter_mut() {
            let from = from.min(out.len());
            out[from..].fill(0.0);
        }
    }
}

enum Cycle {
    Done,
    /// The host was still busy with an earlier cycle, or did not finish this one in time.
    Late,
}

/// What the audio path keeps between cycles. Everything is allocated up front.
struct Rt {
    tracked: Vec<TrackedParam>,
    /// Index into `tracked` of the next dirty param to resend after a reload; `None` when
    /// nothing is pending.
    resend: Option<usize>,
    reset_pending: bool,
    /// Events not delivered yet (the host was busy, or a cycle's block was full). They go
    /// first in the next cycle, at frame 0, so a note-off is late rather than lost.
    carry: EventList,
    /// A cycle missed its deadline. When the host finishes it, its output events are still
    /// collected; its audio is too late to use.
    late_pending: bool,
}

impl Rt {
    fn new() -> Self {
        Self {
            tracked: Vec::new(),
            resend: None,
            reset_pending: false,
            carry: EventList::with_capacity(EVENT_CAPACITY as usize),
            late_pending: false,
        }
    }

    fn track(&mut self, param: ParamId, value: f64) {
        if let Ok(i) = self.tracked.binary_search_by_key(&param, |t| t.id) {
            self.tracked[i].value = value;
            self.tracked[i].dirty = true;
        }
    }

    /// Keeps `e` for the next cycle. Real-time safe; drops it only if the carry is full.
    fn carry(&mut self, mut e: Event) {
        e.frame = 0;
        let _ = self.carry.push(e);
    }
}

/// Appends the host's output events, frames shifted by `offset`, to `ctx`.
fn collect_events(shm: &Shm, ctx: &mut ProcessContext, offset: usize) {
    let h = shm.header();
    let cap = shm.layout().event_capacity as usize;
    // SAFETY: the state word is DONE, so the buffers are ours.
    let records = unsafe { shm.events_out() };
    for i in 0..(h.events_out.load(Ordering::Relaxed) as usize).min(cap) {
        if let Some(mut e) = decode_event(shm::record(records, i)) {
            e.frame = e.frame.saturating_add(offset as u32);
            let _ = ctx.events_out.push(e);
        }
    }
}

/// Runs frames `offset..offset + n` of `ctx` through the host. On [`Cycle::Late`] this chunk's
/// events may not have been taken from `events`; the caller carries the rest. Real-time safe.
#[allow(clippy::too_many_arguments)]
fn run_cycle(
    shm: &Shm,
    ctx: &mut ProcessContext,
    offset: usize,
    n: usize,
    last: bool,
    events: &mut std::iter::Peekable<impl Iterator<Item = Event>>,
    rt: &mut Rt,
    timeout: Duration,
) -> Cycle {
    let h = shm.header();
    let state = h.state.load(Ordering::Acquire);
    if state == REQUEST {
        return Cycle::Late;
    }
    if std::mem::take(&mut rt.late_pending) && state == DONE {
        collect_events(shm, ctx, offset);
    }
    let layout = shm.layout();

    for ch in 0..layout.audio_in as usize {
        // SAFETY: the state word is not REQUEST, so the buffers are ours; one borrow at a time.
        let dst = unsafe { shm.audio_in(ch, n) };
        match ctx.audio_in.get(ch) {
            Some(src) if src.len() >= offset + n => dst.copy_from_slice(&src[offset..offset + n]),
            _ => dst.fill(0.0),
        }
    }

    // SAFETY: as above.
    let records = unsafe { shm.events_in() };
    let cap = layout.event_capacity as usize;
    let mut count = 0;
    // Carried events first; the carry never holds more than a block.
    for e in rt.carry.iter() {
        if count < cap {
            encode_event(e, shm::record(records, count));
            count += 1;
        }
    }
    let carried = count > 0;
    rt.carry.clear();
    // Then parameter values to restore after a reload, at most half a block per cycle so new
    // events still fit. They wait while carried events are being caught up.
    if let Some(start) = rt.resend.filter(|_| !carried) {
        let mut i = start;
        while i < rt.tracked.len() && count < cap / 2 {
            let t = rt.tracked[i];
            if t.dirty {
                let e = Event {
                    frame: 0,
                    kind: EventKind::ParamValue {
                        param: t.id,
                        value: t.value,
                    },
                };
                encode_event(&e, shm::record(records, count));
                count += 1;
            }
            i += 1;
        }
        rt.resend = (i < rt.tracked.len()).then_some(i);
    }
    while let Some(mut e) = events.next_if(|e| last || (e.frame as usize) < offset + n) {
        if count < cap {
            e.frame = (e.frame as usize).saturating_sub(offset).min(n - 1) as u32;
            encode_event(&e, shm::record(records, count));
            count += 1;
        } else {
            rt.carry(e);
        }
    }
    h.events_in.store(count as u32, Ordering::Relaxed);

    h.frames.store(n as u32, Ordering::Relaxed);
    h.write_transport(&ctx.transport.advanced(offset as u32));
    if std::mem::take(&mut rt.reset_pending) {
        h.flags.fetch_or(FLAG_RESET, Ordering::Relaxed);
    }

    let deadline = Instant::now() + timeout;
    h.state.store(REQUEST, Ordering::Release);
    futex_wake(&h.state);
    loop {
        let state = h.state.load(Ordering::Acquire);
        if state == DONE {
            break;
        }
        let now = Instant::now();
        if now >= deadline {
            rt.late_pending = true;
            return Cycle::Late;
        }
        futex_wait(&h.state, state, deadline - now);
    }

    for (ch, out) in ctx.audio_out.iter_mut().enumerate() {
        let out = &mut out[offset..offset + n];
        if ch < layout.audio_out as usize {
            // SAFETY: the state word is DONE, so the buffers are ours again.
            out.copy_from_slice(unsafe { shm.audio_out(ch, n) });
        } else {
            out.fill(0.0);
        }
    }
    collect_events(shm, ctx, offset);
    Cycle::Done
}

impl Node for SandboxedInstance {
    fn ports(&self) -> PortLayout {
        self.ports.clone()
    }

    fn prepare(&mut self, sample_rate: f64, max_frames: u32) {
        self.sample_rate = sample_rate;
        self.max_frames = max_frames.max(1);
        self.layout_changed = false;
        let settings = (sample_rate, self.max_frames);
        // Skipping this is safe because a layout change clears `prepared_with`, and one the
        // host reported that `on_main_thread` has not picked up yet is still in the block.
        let reported = self
            .running()
            .is_some_and(|link| link.shm.header().layout_changed.load(Ordering::Acquire) != 0);
        if self.prepared_with.take() == Some(settings) && !reported {
            self.prepared_with = Some(settings);
            return;
        }
        let Some(link) = self.running() else {
            return;
        };
        let generation = link.generation;
        match self.prepare_host() {
            Ok(()) => self.prepared_with = Some(settings),
            Err(e) => {
                // `prepare` cannot fail; a host that cannot prepare is as good as crashed.
                if self
                    .shared
                    .crash(generation, CrashReason::Lost(e.to_string()))
                    && let Some(link) = &self.link
                {
                    kill(&link.child);
                }
            }
        }
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        let total = ctx.frames as usize;
        for e in ctx.events_in.iter() {
            if let EventKind::ParamValue { param, value } = e.kind {
                self.rt.track(param, value);
            }
        }
        let Some(link) = self.link.as_ref() else {
            Self::silence(ctx, 0);
            return;
        };
        if self.shared.crashed.load(Ordering::Acquire) {
            Self::silence(ctx, 0);
            return;
        }
        let shm = link.shm.clone();
        let max = shm.layout().max_frames as usize;
        let events_in = ctx.events_in;
        let mut events = events_in.iter().copied().peekable();
        let first_out = ctx.events_out.len();
        let mut offset = 0;
        while offset < total {
            let n = (total - offset).min(max);
            let timeout = self
                .config
                .process_timeout
                .unwrap_or_else(|| Duration::from_secs_f64(n as f64 / self.sample_rate.max(1.0)));
            let last = offset + n == total;
            match run_cycle(
                &shm,
                ctx,
                offset,
                n,
                last,
                &mut events,
                &mut self.rt,
                timeout,
            ) {
                Cycle::Done => offset += n,
                Cycle::Late => {
                    self.shared.late_cycles.fetch_add(1, Ordering::Relaxed);
                    Self::silence(ctx, offset);
                    for e in events.by_ref() {
                        self.rt.carry(e);
                    }
                    break;
                }
            }
        }
        // Parameter changes the plugin made (its editor, say) are restored on reload too.
        for e in ctx.events_out.iter().skip(first_out) {
            if let EventKind::ParamValue { param, value } = e.kind {
                self.rt.track(param, value);
            }
        }
    }

    fn latency(&self) -> u32 {
        self.latency
    }

    fn reset(&mut self) {
        self.rt.reset_pending = true;
    }
}

impl PluginInstance for SandboxedInstance {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.shared.descriptor
    }

    /// The list from when the plugin was loaded, refreshed after `load_state`, so it costs no
    /// round trip to the host.
    fn params(&self) -> Vec<ParamInfo> {
        self.params.clone()
    }

    /// Asks the host, which is a round trip: a UI should not poll this for every frame.
    fn param_value(&self, id: ParamId) -> f64 {
        if let Ok((Reply::Value(v), _, _)) = self.control(Request::ParamValue(id), &[]) {
            return v;
        }
        if let Ok(i) = self.rt.tracked.binary_search_by_key(&id, |t| t.id) {
            return self.rt.tracked[i].value;
        }
        self.params
            .iter()
            .find(|p| p.id == id)
            .map_or(0.0, |p| p.default)
    }

    /// While the host is down, returns the last state saved or loaded, so a project save
    /// during a crash keeps what was there.
    fn save_state(&mut self) -> Result<Vec<u8>, PluginError> {
        if self.running().is_none() {
            return self
                .state
                .clone()
                .ok_or_else(|| PluginError::State(self.not_running()));
        }
        match self.control_with(Request::SaveState, &[], self.config.load_timeout)? {
            (Reply::State, state, _) => {
                self.state = Some(state.clone());
                self.mark_saved();
                Ok(state)
            }
            (other, _, _) => Err(PluginError::State(format!("unexpected reply {other:?}"))),
        }
    }

    /// While the host is down, the state is kept and applied on reload. So is a state the host
    /// dies or hangs while loading, though that still returns the error.
    fn load_state(&mut self, data: &[u8]) -> Result<(), PluginError> {
        if !blob_fits(data.len()) {
            return Err(PluginError::State(format!(
                "the plugin state is too large to transfer ({} bytes)",
                data.len()
            )));
        }
        if self.running().is_none() {
            self.state = Some(data.to_vec());
            self.mark_clean();
            return Ok(());
        }
        let latency = match self.control_with(Request::LoadState, data, self.config.load_timeout) {
            Ok((_, _, latency)) => latency,
            Err(e) => {
                // A plugin error leaves the host running and the old state in place.
                if self.running().is_none() {
                    self.state = Some(data.to_vec());
                    self.mark_clean();
                }
                return Err(e);
            }
        };
        self.state = Some(data.to_vec());
        self.mark_clean();
        self.latency = latency;
        // Loading state can change the parameter list (a different preset of a modular synth).
        self.refresh_params();
        Ok(())
    }

    fn has_editor(&self) -> bool {
        self.has_editor
    }

    fn open_editor(&mut self) -> Result<(), PluginError> {
        self.control(Request::OpenEditor, &[])
            .map_err(|e| match e {
                PluginError::Load(m) => PluginError::Editor(m),
                e => e,
            })?;
        self.editor_open = true;
        Ok(())
    }

    fn close_editor(&mut self) {
        self.editor_open = false;
        let _ = self.control(Request::CloseEditor, &[]);
    }

    /// The host pumps the plugin's own main thread; this performs requested reloads and picks
    /// up layout changes the host reported.
    fn on_main_thread(&mut self) {
        if self.shared.reload_requested.load(Ordering::Acquire) {
            let _ = self.reload();
        }
        if let Some(link) = self.running()
            && link.shm.header().layout_changed.swap(0, Ordering::AcqRel) != 0
        {
            self.layout_changed = true;
            self.prepared_with = None;
        }
    }

    fn take_layout_changed(&mut self) -> bool {
        std::mem::take(&mut self.layout_changed)
    }
}

impl Drop for SandboxedInstance {
    fn drop(&mut self) {
        let graceful = self.running().is_some();
        if let Some(link) = self.link.take() {
            link.close(graceful, Duration::from_millis(500));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};

    use daw_core::plugin::PluginFormatKind;

    use super::*;

    fn shared() -> Arc<Shared> {
        Arc::new(Shared {
            descriptor: PluginDescriptor {
                format: PluginFormatKind::Clap,
                id: "test".into(),
                name: "test".into(),
                vendor: String::new(),
                path: "/test".into(),
                is_instrument: false,
                ports: (0, 0),
            },
            crashed: AtomicBool::new(false),
            reason: Mutex::new(None),
            generation: AtomicU64::new(1),
            reload_requested: AtomicBool::new(false),
            pid: AtomicU32::new(0),
            late_cycles: AtomicU64::new(0),
            on_crash: None,
            notified: AtomicU64::new(0),
        })
    }

    #[test]
    fn a_stall_while_the_host_main_thread_is_in_the_plugin_gets_the_call_timeout() {
        let child = Arc::new(Mutex::new(Command::new("sleep").arg("60").spawn().unwrap()));
        let layout = Layout {
            audio_in: 0,
            audio_out: 0,
            max_frames: 64,
            event_capacity: 16,
        };
        let (shm, path) = Shm::create(&std::env::temp_dir(), layout).unwrap();
        std::fs::remove_file(path).unwrap();
        let shm = Arc::new(shm);
        // A cycle is waiting on the plugin lock, which the host's main thread holds.
        shm.header().state.store(REQUEST, Ordering::Release);
        shm.header().main_in_plugin.store(1, Ordering::SeqCst);
        let watched = Arc::new(Mutex::new(shm.clone()));
        let stop = Arc::new(AtomicBool::new(false));
        let shared = shared();
        let dog = {
            let (child, watched, stop, shared) =
                (child.clone(), watched.clone(), stop.clone(), shared.clone());
            std::thread::spawn(move || {
                let hang = Duration::from_millis(20);
                let call = Duration::from_secs(3600);
                watchdog(&child, &watched, &stop, &shared, 1, hang, call);
            })
        };
        // Many hang timeouts pass without a kill.
        std::thread::sleep(Duration::from_millis(300));
        assert!(!shared.crashed.load(Ordering::Acquire));
        // Once the main thread is out of the plugin, the stall is a hang.
        shm.header().main_in_plugin.store(0, Ordering::SeqCst);
        let start = Instant::now();
        while !shared.crashed.load(Ordering::Acquire) {
            assert!(
                start.elapsed() < Duration::from_secs(30),
                "hang not noticed"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(*shared.reason(), Some(CrashReason::Hung));
        stop.store(true, Ordering::Release);
        dog.join().unwrap();
        kill(&child);
    }

    #[test]
    fn a_host_that_trickles_its_reply_cannot_outlast_the_call_timeout() {
        let (app, mut host) = UnixStream::pair().unwrap();
        let trickle = std::thread::spawn(move || {
            let mut request = [0u8; 256];
            let _ = host.read(&mut request);
            // Announce a large reply, then send it a byte at a time, each well inside the
            // timeout, for far longer than the timeout.
            let mut head = Vec::new();
            head.extend_from_slice(&4096u32.to_le_bytes());
            head.extend_from_slice(&(64u32 << 20).to_le_bytes());
            if host.write_all(&head).is_err() {
                return;
            }
            for _ in 0..400 {
                std::thread::sleep(Duration::from_millis(50));
                if host.write_all(b" ").is_err() {
                    return;
                }
            }
        });
        let start = Instant::now();
        let result = call(&app, &Request::Params, &[], Duration::from_millis(200));
        let elapsed = start.elapsed();
        drop(app);
        trickle.join().unwrap();
        assert!(result.is_err());
        // The trickle lasts 20 s; the call must give up long before.
        assert!(elapsed < Duration::from_secs(10), "{elapsed:?}");
    }

    #[test]
    fn tracking_a_new_param_list_keeps_what_was_tracked_for_params_still_in_it() {
        let param = |id, automatable| ParamInfo {
            id,
            name: String::new(),
            min: 0.0,
            max: 1.0,
            default: 0.5,
            automatable,
        };
        let old = [
            TrackedParam {
                id: 1,
                value: 0.25,
                dirty: true,
            },
            TrackedParam {
                id: 2,
                value: 0.75,
                dirty: true,
            },
        ];
        let tracked = tracked_params(&[param(120, true), param(1, true), param(7, false)], &old);
        let got: Vec<_> = tracked.iter().map(|t| (t.id, t.value, t.dirty)).collect();
        assert_eq!(got, [(1, 0.25, true), (120, 0.5, false)]);
    }
}
