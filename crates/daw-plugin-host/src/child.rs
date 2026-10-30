//! The host process side: loads one plugin, answers control requests on the main thread, pumps
//! the plugin's main-thread callbacks, and runs audio cycles on its own audio thread.

use std::ffi::OsString;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use daw_core::event::EventList;
use daw_core::node::ProcessContext;
use daw_core::plugin::{PluginError, PluginInstance};
use daw_plugins::PluginRegistry;

use crate::protocol::{
    FrameReader, InstanceInfo, Reply, Request, Response, WireError, WireParam, blob_fits,
    write_frame,
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
        prepared: (0.0, 0),
        rt_priority: None,
    };
    let result = host.run();
    host.stop_audio();
    // Destroy the plugin here, on the thread that created it.
    drop(lock(&host.plugin).take());
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
    /// Sample rate and max frames the plugin was last prepared with.
    prepared: (f64, u32),
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
                if let Some(p) = lock(&self.plugin).as_mut() {
                    p.on_main_thread();
                }
                last_pump = Instant::now();
            }
        }
    }

    fn handle(&mut self, request: Request, blob: &[u8]) -> (Reply, Vec<u8>) {
        // Calls that can take long make audio cycles skip (output silence) rather than wait.
        let slow = matches!(
            request,
            Request::SaveState | Request::LoadState | Request::OpenEditor | Request::CloseEditor
        );
        let busy = self.audio.as_ref().map(|a| a.shm.clone()).filter(|_| slow);
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
                            self.prepared = (sample_rate, max_frames);
                            Reply::Instantiated(info)
                        })
                }
            }
            Request::Prepare {
                sample_rate,
                max_frames,
                shm,
            } => self.prepare(sample_rate, max_frames, shm.as_deref()),
            Request::Params => self.with_plugin(|p| {
                Ok(Reply::Params(
                    p.params().iter().map(WireParam::from).collect(),
                ))
            }),
            Request::ParamValue(id) => self.with_plugin(|p| Ok(Reply::Value(p.param_value(id)))),
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
        match lock(&self.plugin).as_mut() {
            Some(p) => f(p.as_mut()),
            None => Err(PluginError::Load("no plugin loaded".into())),
        }
    }

    fn prepare(
        &mut self,
        sample_rate: f64,
        max_frames: u32,
        shm: Option<&Path>,
    ) -> Result<Reply, PluginError> {
        if lock(&self.plugin).is_none() {
            return Err(PluginError::Load("no plugin loaded".into()));
        }
        // No audio cycle may run while the plugin is re-prepared or the block changes.
        let old = self.stop_audio();
        let block = match shm {
            Some(path) => Some(Arc::new(Shm::open(path).map_err(|e| {
                PluginError::Load(format!("cannot map the audio block: {e}"))
            })?)),
            None => old,
        };
        if self.prepared != (sample_rate, max_frames) {
            if let Some(p) = lock(&self.plugin).as_mut() {
                p.prepare(sample_rate, max_frames);
            }
            self.prepared = (sample_rate, max_frames);
        }
        if let Some(block) = block {
            self.audio = Some(AudioThread::start(
                block,
                self.plugin.clone(),
                self.rt_priority,
            ));
        }
        Ok(Reply::Ok)
    }

    /// Stops the audio thread, if any, and returns its block.
    fn stop_audio(&mut self) -> Option<Arc<Shm>> {
        self.audio.take().map(AudioThread::stop)
    }
}

struct AudioThread {
    quit: Arc<AtomicBool>,
    shm: Arc<Shm>,
    handle: JoinHandle<()>,
}

impl AudioThread {
    fn start(shm: Arc<Shm>, plugin: SharedPlugin, rt_priority: Option<i32>) -> Self {
        let quit = Arc::new(AtomicBool::new(false));
        let handle = {
            let (shm, quit) = (shm.clone(), quit.clone());
            std::thread::Builder::new()
                .name("plugin-audio".into())
                .spawn(move || audio_loop(&shm, &plugin, &quit, rt_priority))
                .expect("spawn the audio thread")
        };
        Self { quit, shm, handle }
    }

    fn stop(self) -> Arc<Shm> {
        self.quit.store(true, Ordering::Release);
        futex_wake(&self.shm.header().state);
        let _ = self.handle.join();
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

fn audio_loop(shm: &Shm, plugin: &SharedPlugin, quit: &AtomicBool, rt_priority: Option<i32>) {
    if let Some(p) = rt_priority {
        set_realtime(p);
    }
    let layout = shm.layout();
    let h = shm.header();
    let cap = layout.event_capacity as usize;
    let mut events_in = EventList::with_capacity(cap);
    let mut events_out = EventList::with_capacity(cap);
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
            // While the main thread is in a slow call, skip the cycle instead of waiting on it.
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
                    let mut ctx = ProcessContext {
                        frames: frames as u32,
                        transport: &transport,
                        audio_in: &ins[..n_in],
                        audio_out: &mut outs[..n_out],
                        events_in: &events_in,
                        events_out: &mut events_out,
                    };
                    p.process(&mut ctx);
                }
                None => outs[..n_out].iter_mut().for_each(|o| o.fill(0.0)),
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
