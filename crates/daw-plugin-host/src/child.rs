//! The host process side: loads one plugin, answers control requests on the main thread, pumps
//! the plugin's main-thread callbacks, and runs audio cycles on its own audio thread.

use std::cell::Cell;
use std::ffi::OsString;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use daw_core::event::{Event, EventList, ParamId};
use daw_core::node::ProcessContext;
use daw_core::plugin::{PluginError, PluginInstance, PluginProcessor};
use daw_plugins::PluginRegistry;

use crate::protocol::{
    FrameReader, InstanceInfo, Reply, Request, Response, WireError, WireParam, WirePorts,
    blob_fits, write_frame,
};
use crate::shm::{
    self, DONE, FLAG_RESET, MAX_CHANNELS, REQUEST, Shm, decode_event, encode_event, futex_wait,
    futex_wake,
};

/// The argument that selects host mode: `--plugin-host <socket>`.
pub const HOST_ARG: &str = "--plugin-host";

/// How often the plugin's main-thread callbacks run (about 60 Hz).
const PUMP_INTERVAL: Duration = Duration::from_millis(16);

type SharedPlugin = Arc<Mutex<Option<Box<dyn PluginInstance>>>>;

/// The host side of a sandbox. If `args` (without the program name) contain
/// `--plugin-host <socket>`, connects to the socket, hosts one plugin until the application
/// says to stop or goes away, and returns `Some` exit code. Otherwise returns `None`, so an
/// application can call this first thing in `main` and carry on normally.
pub fn run_host_child(
    registry: &PluginRegistry,
    args: impl IntoIterator<Item = OsString>,
) -> Option<ExitCode> {
    let args: Vec<OsString> = args.into_iter().collect();
    let pos = args.iter().position(|a| a == HOST_ARG)?;
    let Some(socket) = args.get(pos + 1) else {
        eprintln!("usage: {HOST_ARG} <socket>");
        return Some(ExitCode::from(2));
    };
    // A panic anywhere (the plugin's adapter, a poisoned lock) must end the process, so the
    // application sees a crash rather than a host that stopped answering.
    std::panic::set_hook(Box::new(|info| {
        eprintln!("daw-plugin-host: {info}");
        std::process::abort();
    }));
    exit_with_parent();
    let stream = match UnixStream::connect(Path::new(socket)) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("daw-plugin-host: cannot connect to {socket:?}: {e}");
            return Some(ExitCode::FAILURE);
        }
    };
    let mut host = Host {
        registry,
        stream,
        plugin: Arc::new(Mutex::new(None)),
        audio: None,
        layout_changed: Cell::new(false),
        state_changed: Cell::new(false),
        editor_open: Cell::new(false),
        rt_priority: None,
    };
    let result = host.run();
    host.stop_audio();
    // Destroy the plugin here, on the thread that created it, then let its format clean up.
    drop(lock(&host.plugin).take());
    registry.shutdown();
    Some(match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("daw-plugin-host: {e}");
            ExitCode::FAILURE
        }
    })
}

/// Exits when the application goes away, even if the main thread is stuck inside the plugin
/// and never sees the socket close. (`PR_SET_PDEATHSIG` would fire when the application thread
/// that started us exits, which can be long before the application does.)
fn exit_with_parent() {
    // SAFETY: getppid has no preconditions.
    let parent = unsafe { libc::getppid() };
    let _ = std::thread::Builder::new()
        .name("parent-watch".into())
        .spawn(move || {
            loop {
                std::thread::sleep(Duration::from_millis(200));
                // SAFETY: as above.
                if unsafe { libc::getppid() } != parent {
                    // SAFETY: ends the process without running exit handlers, which could
                    // block on whatever the stuck plugin holds.
                    unsafe { libc::_exit(1) };
                }
            }
        });
}

fn lock(plugin: &SharedPlugin) -> MutexGuard<'_, Option<Box<dyn PluginInstance>>> {
    plugin.lock().unwrap_or_else(PoisonError::into_inner)
}

struct Host<'a> {
    registry: &'a PluginRegistry,
    stream: UnixStream,
    plugin: SharedPlugin,
    audio: Option<AudioThread>,
    /// The plugin asked to be prepared again, and the application has not been told yet.
    layout_changed: Cell<bool>,
    /// The plugin reported a state change, and the application has not been told yet.
    state_changed: Cell<bool>,
    /// Whether the plugin's editor is open, as last seen.
    editor_open: Cell<bool>,
    rt_priority: Option<i32>,
}

impl Host<'_> {
    fn run(&mut self) -> std::io::Result<()> {
        self.stream.set_read_timeout(Some(PUMP_INTERVAL))?;
        let mut reader = FrameReader::default();
        let mut last_pump = Instant::now();
        loop {
            match reader.poll::<Request>(&self.stream) {
                Ok(Some((request, blob))) => {
                    let shutdown = matches!(request, Request::Shutdown);
                    let (mut reply, mut blob) = if shutdown {
                        (Reply::Ok, Vec::new())
                    } else {
                        self.handle(request, &blob)
                    };
                    if !blob_fits(blob.len()) {
                        reply = Reply::Err(WireError::State(format!(
                            "the plugin state is too large to transfer ({} bytes)",
                            blob.len()
                        )));
                        blob = Vec::new();
                    }
                    let latency = lock(&self.plugin).as_ref().map_or(0, |p| p.latency());
                    // Before the reply, so the application sees an editor it opened as open.
                    if !shutdown {
                        self.publish_layout_change();
                    }
                    write_frame(&self.stream, &Response { latency, reply }, &blob)?;
                    if shutdown {
                        return Ok(());
                    }
                }
                Ok(None) => {}
                // The application closed the socket: it is done with us, or it died.
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(()),
                Err(e) => return Err(e),
            }
            if last_pump.elapsed() >= PUMP_INTERVAL {
                if let Some(p) = self.lock_inside().as_mut() {
                    p.on_main_thread();
                    self.note(p.as_mut());
                }
                self.publish_layout_change();
                last_pump = Instant::now();
            }
        }
    }

    /// Picks up what the plugin reported since the last call.
    fn note(&self, p: &mut dyn PluginInstance) {
        if p.take_layout_changed() {
            self.layout_changed.set(true);
        }
        if p.take_state_changed() {
            self.state_changed.set(true);
        }
        self.editor_open.set(p.is_editor_open());
    }

    /// Tells the application, through the audio block, that the plugin wants to be prepared
    /// again or changed its state, and whether its editor is open. Without a block yet, the
    /// reports wait for one.
    fn publish_layout_change(&self) {
        let Some(audio) = &self.audio else {
            return;
        };
        let header = audio.shm.header();
        if self.layout_changed.get() {
            header.layout_changed.store(1, Ordering::Release);
            self.layout_changed.set(false);
        }
        if self.state_changed.get() {
            header.state_changed.store(1, Ordering::Release);
            self.state_changed.set(false);
        }
        header
            .editor_open
            .store(u32::from(self.editor_open.get()), Ordering::Release);
    }

    /// Locks the plugin for the main thread, flagged in the audio block while held, unless
    /// the audio thread runs a split half, which never waits for it.
    fn lock_inside(&self) -> InsidePlugin<'_> {
        let shm = self
            .audio
            .as_ref()
            .filter(|a| !a.split)
            .map(|a| a.shm.clone());
        InsidePlugin::lock(&self.plugin, shm)
    }

    fn handle(&mut self, request: Request, blob: &[u8]) -> (Reply, Vec<u8>) {
        // Calls that can take long make audio cycles skip (output silence) rather than wait.
        let slow = matches!(
            request,
            Request::SaveState | Request::LoadState | Request::OpenEditor | Request::CloseEditor
        );
        // A split plugin's audio never waits for this thread, so there is nothing to flag.
        let busy = self
            .audio
            .as_ref()
            .filter(|a| !a.split)
            .map(|a| a.shm.clone())
            .filter(|_| slow);
        if let Some(shm) = &busy {
            shm.header().main_busy.store(1, Ordering::SeqCst);
        }
        let result = self.handle_inner(request, blob);
        if let Some(shm) = &busy {
            shm.header().main_busy.store(0, Ordering::SeqCst);
        }
        result
    }

    fn handle_inner(&mut self, request: Request, blob: &[u8]) -> (Reply, Vec<u8>) {
        let none = Vec::new();
        let result = match request {
            Request::Instantiate {
                descriptor,
                sample_rate,
                max_frames,
                rt_priority,
            } => {
                if lock(&self.plugin).is_some() {
                    Err(PluginError::Load("this host already has a plugin".into()))
                } else {
                    self.rt_priority = rt_priority;
                    self.registry
                        .instantiate(&descriptor, sample_rate, max_frames)
                        .map(|p| {
                            let info = InstanceInfo {
                                ports: (&p.ports()).into(),
                                params: p.params().iter().map(WireParam::from).collect(),
                                has_editor: p.has_editor(),
                            };
                            *lock(&self.plugin) = Some(p);
                            Reply::Instantiated(info)
                        })
                }
            }
            Request::Prepare {
                sample_rate,
                max_frames,
                shm,
                prepare,
            } => self.prepare(sample_rate, max_frames, shm.as_deref(), prepare),
            Request::Params => self.with_plugin(|p| {
                Ok(Reply::Params(
                    p.params().iter().map(WireParam::from).collect(),
                ))
            }),
            Request::ParamValue(id) => self.with_plugin(|p| value_reply(id, p.param_value(id))),
            Request::SaveState => {
                return match self.with_plugin(|p| p.save_state()) {
                    Ok(state) => (Reply::State, state),
                    Err(e) => (Reply::Err((&e).into()), none),
                };
            }
            Request::LoadState => self.with_plugin(|p| p.load_state(blob).map(|()| Reply::Ok)),
            Request::OpenEditor => self.with_plugin(|p| p.open_editor().map(|()| Reply::Ok)),
            Request::CloseEditor => self.with_plugin(|p| {
                p.close_editor();
                Ok(Reply::Ok)
            }),
            Request::SetEditorTitle(title) => self.with_plugin(|p| {
                p.set_editor_title(&title);
                Ok(Reply::Ok)
            }),
            Request::Shutdown => Ok(Reply::Ok),
        };
        match result {
            Ok(reply) => (reply, none),
            Err(e) => (Reply::Err((&e).into()), none),
        }
    }

    fn with_plugin<T>(
        &self,
        f: impl FnOnce(&mut dyn PluginInstance) -> Result<T, PluginError>,
    ) -> Result<T, PluginError> {
        match self.lock_inside().as_mut() {
            Some(p) => {
                let result = f(p.as_mut());
                // Loading state, say, can change the plugin's layout.
                self.note(p.as_mut());
                result
            }
            None => Err(PluginError::Load("no plugin loaded".into())),
        }
    }

    fn prepare(
        &mut self,
        sample_rate: f64,
        max_frames: u32,
        shm: Option<&Path>,
        prepare: bool,
    ) -> Result<Reply, PluginError> {
        if lock(&self.plugin).is_none() {
            return Err(PluginError::Load("no plugin loaded".into()));
        }
        // No audio cycle may run while the plugin is re-prepared or the block changes.
        let old = self.stop_audio();
        let block = match shm {
            Some(path) => {
                let new =
                    Arc::new(Shm::open(path).map_err(|e| {
                        PluginError::Load(format!("cannot map the audio block: {e}"))
                    })?);
                // A change reported on the old block and not yet taken carries over.
                if let Some(old) = &old {
                    let (old, fresh) = (old.header(), new.header());
                    if old.layout_changed.load(Ordering::Acquire) != 0 {
                        fresh.layout_changed.store(1, Ordering::Release);
                    }
                    if old.state_changed.load(Ordering::Acquire) != 0 {
                        fresh.state_changed.store(1, Ordering::Release);
                    }
                }
                Some(new)
            }
            None => old,
        };
        // The audio thread is stopped, so flag the block it restarts with: a cycle the
        // application posts meanwhile waits for this.
        let ports = {
            let mut inside = InsidePlugin::lock(&self.plugin, block.clone());
            let p = inside.as_mut().expect("checked above");
            if prepare {
                p.prepare(sample_rate, max_frames);
                // This prepare is the one the plugin asked for.
                p.take_layout_changed();
                self.layout_changed.set(false);
                if let Some(block) = &block {
                    block.header().layout_changed.store(0, Ordering::Release);
                }
            }
            WirePorts::from(&p.ports())
        };
        if let Some(block) = block {
            self.audio = Some(AudioThread::start(
                block,
                self.plugin.clone(),
                self.rt_priority,
            ));
        }
        Ok(Reply::Prepared(ports))
    }

    /// Stops the audio thread, if any, and returns its block.
    fn stop_audio(&mut self) -> Option<Arc<Shm>> {
        self.audio.take().map(AudioThread::stop)
    }
}

/// The plugin locked by the host's main thread. [`Header::main_in_plugin`] is set only once
/// the lock is held, and cleared before it is released: a main thread still waiting for an
/// audio thread stuck in the plugin must leave the hang timeout in force.
///
/// [`Header::main_in_plugin`]: crate::shm::Header::main_in_plugin
struct InsidePlugin<'a> {
    guard: MutexGuard<'a, Option<Box<dyn PluginInstance>>>,
    shm: Option<Arc<Shm>>,
}

impl<'a> InsidePlugin<'a> {
    fn lock(plugin: &'a SharedPlugin, shm: Option<Arc<Shm>>) -> Self {
        let guard = lock(plugin);
        if let Some(shm) = &shm {
            shm.header().main_in_plugin.store(1, Ordering::SeqCst);
        }
        Self { guard, shm }
    }
}

impl std::ops::Deref for InsidePlugin<'_> {
    type Target = Option<Box<dyn PluginInstance>>;
    fn deref(&self) -> &Self::Target {
        &self.guard
    }
}

impl std::ops::DerefMut for InsidePlugin<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.guard
    }
}

impl Drop for InsidePlugin<'_> {
    // Runs before the guard field drops, so the flag is clear by the time the lock is free.
    fn drop(&mut self) {
        if let Some(shm) = &self.shm {
            shm.header().main_in_plugin.store(0, Ordering::SeqCst);
        }
    }
}

/// JSON has no NaN or infinity, so they would not cross the wire: an infinity is clamped, and
/// NaN (no value) is an error, which makes the application fall back to the last value it knows.
fn value_reply(id: ParamId, value: f64) -> Result<Reply, PluginError> {
    if value.is_nan() {
        return Err(PluginError::Load(format!(
            "the plugin has no value for parameter {id}"
        )));
    }
    Ok(Reply::Value(value.clamp(f64::MIN, f64::MAX)))
}

/// The thread that runs the plugin's audio cycles. A plugin that can
/// [`split`](PluginInstance::split) hands it its audio half for as long as it runs, so the
/// main thread's calls never hold a cycle up; any other plugin is locked for each cycle.
struct AudioThread {
    quit: Arc<AtomicBool>,
    shm: Arc<Shm>,
    plugin: SharedPlugin,
    /// It runs the plugin's split half.
    split: bool,
    handle: JoinHandle<Option<Box<dyn PluginProcessor>>>,
}

impl AudioThread {
    fn start(shm: Arc<Shm>, plugin: SharedPlugin, rt_priority: Option<i32>) -> Self {
        let quit = Arc::new(AtomicBool::new(false));
        let mut half = lock(&plugin).as_mut().and_then(|p| p.split());
        let split = half.is_some();
        let handle = {
            let (shm, quit, plugin) = (shm.clone(), quit.clone(), plugin.clone());
            std::thread::Builder::new()
                .name("plugin-audio".into())
                .spawn(move || {
                    audio_loop(&shm, &plugin, &mut half, &quit, rt_priority);
                    half
                })
                .expect("spawn the audio thread")
        };
        Self {
            quit,
            shm,
            plugin,
            split,
            handle,
        }
    }

    /// Stops the thread and gives the plugin its audio half back.
    fn stop(self) -> Arc<Shm> {
        self.quit.store(true, Ordering::Release);
        futex_wake(&self.shm.header().state);
        // The thread aborts the process if it panics, so it always hands the half back.
        if let Ok(Some(half)) = self.handle.join()
            && let Some(p) = lock(&self.plugin).as_mut()
        {
            p.join(half);
        }
        self.shm
    }
}

fn set_realtime(priority: i32) {
    let param = libc::sched_param {
        sched_priority: priority,
    };
    // SAFETY: plain syscall wrapper on the calling thread with a valid parameter struct.
    let err =
        unsafe { libc::pthread_setschedparam(libc::pthread_self(), libc::SCHED_FIFO, &param) };
    if err != 0 {
        eprintln!(
            "daw-plugin-host: real-time priority {priority} not granted ({}); audio may glitch",
            std::io::Error::from_raw_os_error(err)
        );
    }
}

fn audio_loop(
    shm: &Shm,
    plugin: &SharedPlugin,
    half: &mut Option<Box<dyn PluginProcessor>>,
    quit: &AtomicBool,
    rt_priority: Option<i32>,
) {
    if let Some(p) = rt_priority {
        set_realtime(p);
    }
    let layout = shm.layout();
    let h = shm.header();
    let cap = layout.event_capacity as usize;
    let mut events_in = EventList::with_capacity(cap);
    let mut events_out = EventList::with_capacity(cap);
    // Events of a skipped cycle, delivered at the start of the next one the plugin processes.
    let mut skipped = EventList::with_capacity(cap);
    let n_in = layout.audio_in as usize;
    let n_out = layout.audio_out as usize;
    loop {
        loop {
            if quit.load(Ordering::Acquire) {
                return;
            }
            let state = h.state.load(Ordering::Acquire);
            if state == REQUEST {
                break;
            }
            futex_wait(&h.state, state, Duration::from_millis(100));
        }

        let frames = h.frames.load(Ordering::Relaxed).min(layout.max_frames) as usize;
        let transport = h.read_transport();
        let flags = h.flags.swap(0, Ordering::Relaxed);

        // SAFETY: the state word is REQUEST, so the buffers are ours until we store DONE.
        let records_in = unsafe { shm.events_in() };
        events_in.clear();
        for e in skipped.iter() {
            let _ = events_in.push(*e);
        }
        skipped.clear();
        for i in 0..(h.events_in.load(Ordering::Relaxed) as usize).min(cap) {
            if let Some(mut e) = decode_event(shm::record(records_in, i)) {
                e.frame = e.frame.min(frames.saturating_sub(1) as u32);
                let _ = events_in.push(e);
            }
        }
        events_out.clear();

        {
            let mut ins: [&[f32]; MAX_CHANNELS] = [&[]; MAX_CHANNELS];
            let mut outs: [&mut [f32]; MAX_CHANNELS] = std::array::from_fn(|_| Default::default());
            for (ch, slot) in ins.iter_mut().take(n_in).enumerate() {
                // SAFETY: our turn (see above); each channel is borrowed once.
                *slot = unsafe { shm.audio_in(ch, frames) };
            }
            for (ch, slot) in outs.iter_mut().take(n_out).enumerate() {
                // SAFETY: as above.
                *slot = unsafe { shm.audio_out(ch, frames) };
            }
            let ran = {
                let mut ctx = ProcessContext {
                    frames: frames as u32,
                    transport: &transport,
                    audio_in: &ins[..n_in],
                    audio_out: &mut outs[..n_out],
                    events_in: &events_in,
                    events_out: &mut events_out,
                };
                let ran = match half.as_deref_mut() {
                    Some(half) => {
                        if flags & FLAG_RESET != 0 {
                            half.reset();
                        }
                        half.process(&mut ctx);
                        true
                    }
                    None => {
                        // While the main thread is in a slow call, skip the cycle instead of
                        // waiting on it.
                        let mut guard = if h.main_busy.load(Ordering::SeqCst) != 0 {
                            match plugin.try_lock() {
                                Ok(guard) => Some(guard),
                                Err(std::sync::TryLockError::Poisoned(e)) => Some(e.into_inner()),
                                Err(std::sync::TryLockError::WouldBlock) => None,
                            }
                        } else {
                            Some(lock(plugin))
                        };
                        match guard.as_mut().and_then(|g| g.as_mut()) {
                            Some(p) => {
                                if flags & FLAG_RESET != 0 {
                                    p.reset();
                                }
                                p.process(&mut ctx);
                                true
                            }
                            None => false,
                        }
                    }
                };
                if !ran {
                    ctx.audio_out.iter_mut().for_each(|o| o.fill(0.0));
                }
                ran
            };
            if !ran {
                // The application counts the cycle as done, so keep what it asked for: a
                // note-off or a reset is late rather than lost.
                for e in events_in.iter() {
                    let _ = skipped.push(Event { frame: 0, ..*e });
                }
                if flags & FLAG_RESET != 0 {
                    h.flags.fetch_or(FLAG_RESET, Ordering::Relaxed);
                }
            }
        }

        // SAFETY: still our turn.
        let records_out = unsafe { shm.events_out() };
        let mut count = 0;
        for e in events_out.iter().take(cap) {
            encode_event(e, shm::record(records_out, count));
            count += 1;
        }
        h.events_out.store(count as u32, Ordering::Relaxed);
        h.completed.fetch_add(1, Ordering::Relaxed);
        h.state.store(DONE, Ordering::Release);
        futex_wake(&h.state);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicU32;

    use daw_core::event::EventKind;
    use daw_core::node::{Node, PortLayout};
    use daw_core::plugin::{ParamInfo, PluginDescriptor, PluginFormatKind};

    use super::*;
    use crate::shm::Layout;

    /// Records what reaches it; `on_main_thread` notes whether the host flagged that its main
    /// thread is inside the plugin.
    #[derive(Default)]
    struct Probe {
        shm: Option<Arc<Shm>>,
        in_plugin: Arc<AtomicU32>,
        resets: Arc<AtomicU32>,
        events: Arc<Mutex<Vec<Event>>>,
        value: f64,
        /// An audio half to hand out, and how often it came back.
        half: Option<Box<dyn PluginProcessor>>,
        joins: Arc<AtomicU32>,
    }

    /// A [`Probe`]'s audio half: records the same things.
    struct HalfProbe {
        resets: Arc<AtomicU32>,
        events: Arc<Mutex<Vec<Event>>>,
    }

    impl PluginProcessor for HalfProbe {
        fn process(&mut self, ctx: &mut ProcessContext) {
            self.events
                .lock()
                .unwrap()
                .extend(ctx.events_in.iter().copied());
        }
        fn reset(&mut self) {
            self.resets.fetch_add(1, Ordering::SeqCst);
        }
        fn into_any(self: Box<Self>) -> Box<dyn std::any::Any + Send> {
            self
        }
    }

    impl Node for Probe {
        fn ports(&self) -> PortLayout {
            PortLayout::default()
        }
        fn prepare(&mut self, _sample_rate: f64, _max_frames: u32) {}
        fn process(&mut self, ctx: &mut ProcessContext) {
            self.events
                .lock()
                .unwrap()
                .extend(ctx.events_in.iter().copied());
        }
        fn reset(&mut self) {
            self.resets.fetch_add(1, Ordering::SeqCst);
        }
    }

    impl PluginInstance for Probe {
        fn descriptor(&self) -> &PluginDescriptor {
            static D: std::sync::OnceLock<PluginDescriptor> = std::sync::OnceLock::new();
            D.get_or_init(|| PluginDescriptor {
                format: PluginFormatKind::Clap,
                id: "probe".into(),
                name: "probe".into(),
                vendor: String::new(),
                path: "/probe".into(),
                is_instrument: false,
                ports: (0, 0),
            })
        }
        fn params(&self) -> Vec<ParamInfo> {
            Vec::new()
        }
        fn param_value(&self, _id: u32) -> f64 {
            self.value
        }
        fn save_state(&mut self) -> Result<Vec<u8>, PluginError> {
            Ok(Vec::new())
        }
        fn load_state(&mut self, _data: &[u8]) -> Result<(), PluginError> {
            Ok(())
        }
        fn has_editor(&self) -> bool {
            false
        }
        fn open_editor(&mut self) -> Result<(), PluginError> {
            Ok(())
        }
        fn close_editor(&mut self) {}
        fn split(&mut self) -> Option<Box<dyn PluginProcessor>> {
            self.half.take()
        }
        fn join(&mut self, half: Box<dyn PluginProcessor>) {
            self.half = Some(half);
            self.joins.fetch_add(1, Ordering::SeqCst);
        }
        fn on_main_thread(&mut self) {
            if let Some(shm) = &self.shm {
                let flag = shm.header().main_in_plugin.load(Ordering::SeqCst);
                self.in_plugin.fetch_max(flag + 1, Ordering::SeqCst);
            }
        }
    }

    fn block() -> Arc<Shm> {
        let layout = Layout {
            audio_in: 0,
            audio_out: 0,
            max_frames: 64,
            event_capacity: 16,
        };
        let (shm, path) = Shm::create(&std::env::temp_dir(), layout).unwrap();
        std::fs::remove_file(path).unwrap();
        Arc::new(shm)
    }

    fn shared(probe: Probe) -> SharedPlugin {
        Arc::new(Mutex::new(Some(Box::new(probe))))
    }

    /// Posts one cycle with `events` and waits until the host is done with it.
    fn cycle(shm: &Shm, events: &[Event], flags: u32) {
        let h = shm.header();
        // SAFETY: the state word is not REQUEST, so the buffers are ours.
        let records = unsafe { shm.events_in() };
        for (i, e) in events.iter().enumerate() {
            encode_event(e, shm::record(records, i));
        }
        h.events_in.store(events.len() as u32, Ordering::Relaxed);
        h.frames.store(16, Ordering::Relaxed);
        h.flags.fetch_or(flags, Ordering::Relaxed);
        h.state.store(REQUEST, Ordering::Release);
        futex_wake(&h.state);
        let start = Instant::now();
        loop {
            let state = h.state.load(Ordering::Acquire);
            if state == DONE {
                return;
            }
            assert!(
                start.elapsed() < Duration::from_secs(30),
                "cycle never done"
            );
            futex_wait(&h.state, state, Duration::from_millis(10));
        }
    }

    #[test]
    fn the_main_thread_pump_is_flagged_as_inside_the_plugin() {
        let shm = block();
        let in_plugin = Arc::new(AtomicU32::new(0));
        let plugin = shared(Probe {
            shm: Some(shm.clone()),
            in_plugin: in_plugin.clone(),
            ..Probe::default()
        });
        let registry = PluginRegistry::new();
        let (stream, peer) = UnixStream::pair().unwrap();
        let mut host = Host {
            registry: &registry,
            stream,
            plugin: plugin.clone(),
            audio: Some(AudioThread::start(shm.clone(), plugin, None)),
            layout_changed: Cell::new(false),
            state_changed: Cell::new(false),
            editor_open: Cell::new(false),
            rt_priority: None,
        };
        // The host pumps between reads; close the socket once it has pumped at least once.
        let closer = {
            let in_plugin = in_plugin.clone();
            std::thread::spawn(move || {
                while in_plugin.load(Ordering::SeqCst) == 0 {
                    std::thread::sleep(Duration::from_millis(5));
                }
                drop(peer);
            })
        };
        host.run().unwrap();
        closer.join().unwrap();
        host.stop_audio();
        // 2 means the flag was set while the pump ran.
        assert_eq!(in_plugin.load(Ordering::SeqCst), 2);
        assert_eq!(shm.header().main_in_plugin.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_main_thread_waiting_on_a_stuck_audio_thread_is_not_flagged_as_inside_the_plugin() {
        let shm = block();
        let plugin = shared(Probe::default());
        let registry = PluginRegistry::new();
        let (stream, peer) = UnixStream::pair().unwrap();
        // Holding the lock stands in for an audio thread stuck in process().
        let stuck = lock(&plugin);
        std::thread::scope(|s| {
            let pumping = s.spawn(|| {
                let mut host = Host {
                    registry: &registry,
                    stream,
                    plugin: plugin.clone(),
                    audio: Some(AudioThread::start(shm.clone(), plugin.clone(), None)),
                    layout_changed: Cell::new(false),
                    state_changed: Cell::new(false),
                    editor_open: Cell::new(false),
                    rt_priority: None,
                };
                host.run().unwrap();
                host.stop_audio();
            });
            // Several pump intervals: the pump is blocked on the lock by now.
            let start = Instant::now();
            while start.elapsed() < Duration::from_millis(200) {
                assert_eq!(shm.header().main_in_plugin.load(Ordering::SeqCst), 0);
                std::thread::sleep(Duration::from_millis(5));
            }
            drop(peer);
            drop(stuck);
            pumping.join().unwrap();
        });
        assert_eq!(shm.header().main_in_plugin.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_skipped_cycle_keeps_its_events_and_reset_for_the_next() {
        let shm = block();
        let (resets, events) = (Arc::new(AtomicU32::new(0)), Arc::default());
        let plugin = shared(Probe {
            resets: resets.clone(),
            events: Arc::clone(&events),
            ..Probe::default()
        });
        let audio = AudioThread::start(shm.clone(), plugin.clone(), None);
        let off = Event {
            frame: 3,
            kind: EventKind::NoteOff {
                channel: 0,
                key: Some(60),
                velocity: 0.0,
                note_id: -1,
            },
        };

        // The main thread is in a slow call: the cycle is skipped.
        let guard = lock(&plugin);
        shm.header().main_busy.store(1, Ordering::SeqCst);
        cycle(&shm, &[off], FLAG_RESET);
        shm.header().main_busy.store(0, Ordering::SeqCst);
        drop(guard);
        assert!(events.lock().unwrap().is_empty());
        assert_eq!(resets.load(Ordering::SeqCst), 0);

        // The next cycle the plugin processes gets them, at its start.
        cycle(&shm, &[], 0);
        audio.stop();
        assert_eq!(resets.load(Ordering::SeqCst), 1);
        assert_eq!(*events.lock().unwrap(), [Event { frame: 0, ..off }]);
    }

    #[test]
    fn a_split_plugin_plays_while_the_main_thread_holds_it() {
        let shm = block();
        let (resets, events, joins) = (Arc::new(AtomicU32::new(0)), Arc::default(), Arc::default());
        let half = HalfProbe {
            resets: resets.clone(),
            events: Arc::clone(&events),
        };
        let plugin = shared(Probe {
            half: Some(Box::new(half)),
            joins: Arc::clone(&joins),
            ..Probe::default()
        });
        let audio = AudioThread::start(shm.clone(), plugin.clone(), None);
        let on = Event {
            frame: 3,
            kind: EventKind::NoteOn {
                channel: 0,
                key: 60,
                velocity: 1.0,
                note_id: -1,
            },
        };

        // A long main-thread call holds the instance; the audio half does not need it.
        let guard = lock(&plugin);
        shm.header().main_busy.store(1, Ordering::SeqCst);
        cycle(&shm, &[on], FLAG_RESET);
        assert_eq!(resets.load(Ordering::SeqCst), 1);
        assert_eq!(*events.lock().unwrap(), [on]);
        shm.header().main_busy.store(0, Ordering::SeqCst);
        drop(guard);

        // Stopping the audio thread gives the plugin its half back.
        audio.stop();
        assert_eq!(joins.load(Ordering::SeqCst), 1);
        let audio = AudioThread::start(shm.clone(), plugin.clone(), None);
        cycle(&shm, &[], 0);
        audio.stop();
        assert_eq!(joins.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn non_finite_param_values_reach_the_application_as_something_it_can_read() {
        let registry = PluginRegistry::new();
        let (stream, _peer) = UnixStream::pair().unwrap();
        let mut host = Host {
            registry: &registry,
            stream,
            plugin: Arc::new(Mutex::new(None)),
            audio: None,
            layout_changed: Cell::new(false),
            state_changed: Cell::new(false),
            editor_open: Cell::new(false),
            rt_priority: None,
        };
        for (value, want) in [
            (f64::NAN, None),
            (f64::INFINITY, Some(f64::MAX)),
            (f64::NEG_INFINITY, Some(f64::MIN)),
            (0.5, Some(0.5)),
        ] {
            *lock(&host.plugin) = Some(Box::new(Probe {
                value,
                ..Probe::default()
            }));
            let (reply, _) = host.handle(Request::ParamValue(1), &[]);
            let mut wire = Vec::new();
            write_frame(&mut wire, &Response { latency: 0, reply }, &[]).unwrap();
            let (response, _): (Response, _) = crate::protocol::read_frame(&wire[..]).unwrap();
            match (response.reply, want) {
                (Reply::Value(v), Some(want)) => assert_eq!(v, want),
                (Reply::Err(_), None) => {}
                (other, _) => panic!("{value}: {other:?}"),
            }
        }
    }
}
