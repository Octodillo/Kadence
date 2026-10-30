//! Plugin instances in a graph.
//!
//! A [`PluginInstance`] puts main-thread calls and `process` on one object, and a plugin can
//! ask for a new port layout or latency at any time ([`PluginInstance::take_layout_changed`]).
//! A format that can [`split`](PluginInstance::split) its instances hands the audio thread
//! their audio half, so main-thread calls, however slow, never cost the plugin's audio; for
//! the rest, the two threads take turns on the instance.
//! [`PluginHandle`] owns the instance on the main thread and hands the audio thread a
//! [`PluginNode`] for each graph. The node shares the instance, so state lives in the handle,
//! not in the graph: build every graph with a fresh [`PluginHandle::node`] rather than
//! [`GraphDesc::reuse`](crate::GraphDesc::reuse), and the plugin keeps its state, while each
//! compile picks up its current ports and latency.
//!
//! On the main thread, call [`PluginHandle::pump`] about 60 times a second, along with
//! [`EngineController::collect_garbage`](crate::EngineController::collect_garbage), which
//! frees retired graphs and with them the nodes that would otherwise keep an older node of
//! the plugin quiet. When `pump` returns true, build and send the graph again; [`PluginHandle::node`] then prepares the plugin, and
//! the rest of the graph plays on while that one plugin restarts. A plain rebuild with no
//! change does not prepare it again, so it plays on without a gap.
//!
//! Only one node of a handle plays at a time: the newest one handed out, from the first block
//! it processes. Older nodes, and nodes made for a layout the plugin has since left, output
//! silence, and `pump` keeps returning true while the newest node that played is out of date
//! (say, the next graph failed to compile), so the host rebuilds until a current node plays.
//! Until a new graph is swapped in, that can ask for an extra rebuild, which does not
//! prepare the plugin again. A handle serves one engine; to render offline while the engine
//! runs, instantiate the plugin again.
//!
//! The handle is neither `Send` nor `Sync`: every method, and compiling a graph that holds
//! one of its nodes, runs on the thread that created it, as some formats (CLAP) require.
//! Dropping the handle frees the instance there too, even while a graph still holds nodes.

use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, ThreadId};

use daw_core::exclusive::Exclusive;
use daw_core::node::{Node, PortLayout, ProcessContext};
use daw_core::plugin::{PluginInstance, PluginProcessor};

/// How many times the audio thread retries a plugin the main thread holds before it outputs
/// silence for that block: a few microseconds, enough for a short main-thread call.
const AUDIO_RETRIES: u32 = 100;

/// What the audio thread plays.
enum Audio {
    /// The whole instance, for a format that cannot [`split`](PluginInstance::split): the
    /// main thread takes turns with the audio thread on it.
    Whole(Box<dyn PluginInstance>),
    /// The audio half of a split instance, which lives in [`Main`]. `None` while the main
    /// thread prepares the plugin, or when the prepared plugin has nothing to play.
    Split(Option<Box<dyn PluginProcessor>>),
    /// The handle was dropped.
    Gone,
}

/// One call on whichever half the audio thread holds.
enum Target<'a> {
    Whole(&'a mut dyn PluginInstance),
    Split(&'a mut dyn PluginProcessor),
}

impl Target<'_> {
    fn process(&mut self, ctx: &mut ProcessContext) {
        match self {
            Self::Whole(p) => p.process(ctx),
            Self::Split(p) => p.process(ctx),
        }
    }

    fn reset(&mut self) {
        match self {
            Self::Whole(p) => p.reset(),
            Self::Split(p) => p.reset(),
        }
    }
}

/// What the audio thread and the main thread share; the main thread holds it only briefly
/// for a split instance.
struct Inner {
    audio: Audio,
    /// The node that last played. A newer node takes over on its first block; older ones
    /// stay quiet.
    owner: u64,
}

/// Main-thread state. Only the handle's thread locks it, so the audio thread never waits on
/// it.
struct Main {
    /// A split instance; `None` for a whole one, which lives in [`Audio::Whole`], and once
    /// the handle is dropped.
    instance: Option<Box<dyn PluginInstance>>,
    state: MainState,
}

struct MainState {
    /// Settings of the last `prepare`.
    prepared: (f64, u32),
    /// Ports and latency as of the last `prepare`.
    ports: PortLayout,
    latency: u32,
    /// The plugin asked for another layout and has not been prepared since. Also set while
    /// a prepare runs, so one that panicked is done again.
    layout_changed: bool,
}

impl MainState {
    /// Prepares `p` after every node was retired, and reads what the prepare changed.
    fn prepare(&mut self, p: &mut dyn PluginInstance, sample_rate: f64, max_frames: u32) {
        self.layout_changed = true;
        p.prepare(sample_rate, max_frames);
        self.prepared = (sample_rate, max_frames);
        self.ports = p.ports();
        self.latency = p.latency();
        // The prepare answered whatever the plugin had asked for before it.
        p.take_layout_changed();
        self.layout_changed = false;
    }

    /// Picks up a layout change the plugin reported or a caller made through `with`.
    /// Returns true when no node was compiled for the plugin's ports any more.
    fn note_layout_change(&mut self, p: &mut dyn PluginInstance) -> bool {
        if p.take_layout_changed() {
            self.layout_changed = true;
        }
        if p.latency() != self.latency {
            // Running nodes can go on playing; compensation follows with the next graph.
            self.latency = p.latency();
            self.layout_changed = true;
        }
        if p.ports() != self.ports {
            self.ports = p.ports();
            self.layout_changed = true;
            return true;
        }
        false
    }
}

struct Shared {
    inner: Exclusive<Inner>,
    /// Bumped on every `prepare`: a node built for an older layout must not call the plugin.
    /// Read by the audio thread inside `inner`; bumped by the main thread while it holds
    /// `inner` or, for a split instance, before it takes the audio half back.
    epoch: AtomicU64,
    main: Mutex<Main>,
    thread: ThreadId,
    /// Set while the main thread is inside the plugin, to catch re-entry, which would
    /// deadlock or spin forever.
    main_inside: AtomicBool,
    /// Last node handed out.
    nodes: AtomicU64,
    /// `Inner::owner` while that node lives, 0 once it is dropped: then an older node may
    /// take over.
    live_owner: AtomicU64,
    /// The node asked to play last, if it was the owner or newer, could not: it was made
    /// for an older layout. Written by the audio thread; atomic so `pump` reads it without
    /// stopping the audio.
    stale: AtomicBool,
    /// The node that last played caught up with ports other than the ones it was made, and
    /// wired, with.
    rewire: AtomicBool,
    /// Blocks the audio thread output silence for because the main thread held the plugin.
    busy: AtomicU64,
}

impl Shared {
    /// Runs `f` on the plugin and the main-thread state. A split instance is reached without
    /// stopping the audio thread; a whole one waits for it to leave the plugin.
    fn main<R>(&self, f: impl FnOnce(Option<&mut dyn PluginInstance>, &mut MainState) -> R) -> R {
        assert_eq!(
            thread::current().id(),
            self.thread,
            "a plugin is used on the thread that created its PluginHandle"
        );
        assert!(
            !self.main_inside.swap(true, Ordering::Relaxed),
            "PluginHandle used again from inside one of its own calls"
        );
        struct Leave<'a>(&'a AtomicBool);
        impl Drop for Leave<'_> {
            fn drop(&mut self) {
                self.0.store(false, Ordering::Relaxed);
            }
        }
        let _leave = Leave(&self.main_inside);
        let mut main = self.lock_main();
        let Main { instance, state } = &mut *main;
        match instance {
            Some(p) => f(Some(p.as_mut()), state),
            None => self.inner.main(|i| match &mut i.audio {
                Audio::Whole(p) => f(Some(p.as_mut()), state),
                _ => f(None, state),
            }),
        }
    }

    fn lock_main(&self) -> MutexGuard<'_, Main> {
        self.main.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn retire_nodes(&self) {
        self.epoch.fetch_add(1, Ordering::Release);
    }

    /// Prepares the plugin: takes the audio half back from a split instance, prepares, and
    /// hands out a fresh half. Every node is retired first, so a prepare that panics halfway
    /// leaves none calling the plugin with a layout it was not compiled for.
    fn prepare(&self, sample_rate: f64, max_frames: u32) {
        let split = self.lock_main().instance.is_some();
        if !split {
            self.main(|p, state| {
                if let Some(p) = p {
                    self.retire_nodes();
                    state.prepare(p, sample_rate, max_frames);
                }
            });
            // A plugin that could not split before it was prepared may now.
            let instance = self.inner.main(|i| {
                let Audio::Whole(p) = &mut i.audio else {
                    return None;
                };
                let half = p.split()?;
                match std::mem::replace(&mut i.audio, Audio::Split(Some(half))) {
                    Audio::Whole(p) => Some(p),
                    _ => unreachable!("matched above"),
                }
            });
            if instance.is_some() {
                self.lock_main().instance = instance;
            }
            return;
        }
        let half = self.inner.main(|i| {
            self.retire_nodes();
            match &mut i.audio {
                Audio::Split(half) => half.take(),
                _ => None,
            }
        });
        let half = self.main(|p, state| {
            let p = p.expect("a split instance stays in Main while the handle lives");
            if let Some(half) = half {
                p.join(half);
            }
            state.prepare(p, sample_rate, max_frames);
            p.split()
        });
        self.inner.main(|i| i.audio = Audio::Split(half));
    }
}

/// Owns a plugin instance on the main thread. See the [module docs](self).
pub struct PluginHandle {
    shared: Arc<Shared>,
    _not_send: PhantomData<*const ()>,
}

impl PluginHandle {
    /// Takes an instance fresh from [`PluginFormat::instantiate`] with these settings, which
    /// activates it, so the first graph does not need to restart it.
    ///
    /// [`PluginFormat::instantiate`]: daw_core::plugin::PluginFormat::instantiate
    pub fn new(mut plugin: Box<dyn PluginInstance>, sample_rate: f64, max_frames: u32) -> Self {
        let state = MainState {
            prepared: (sample_rate, max_frames),
            ports: plugin.ports(),
            latency: plugin.latency(),
            layout_changed: false,
        };
        let (audio, instance) = match plugin.split() {
            Some(half) => (Audio::Split(Some(half)), Some(plugin)),
            None => (Audio::Whole(plugin), None),
        };
        Self {
            shared: Arc::new(Shared {
                inner: Exclusive::new(Inner { audio, owner: 0 }),
                epoch: AtomicU64::new(0),
                main: Mutex::new(Main { instance, state }),
                thread: thread::current().id(),
                main_inside: AtomicBool::new(false),
                nodes: AtomicU64::new(0),
                live_owner: AtomicU64::new(0),
                stale: AtomicBool::new(false),
                rewire: AtomicBool::new(false),
                busy: AtomicU64::new(0),
            }),
            _not_send: PhantomData,
        }
    }

    /// Runs `f` on the plugin, for state, parameters and editors. For a format that splits
    /// its instances (CLAP), the audio thread plays on meanwhile.
    /// For one that does not, it waits a few microseconds, then outputs silence for this
    /// plugin, so keep `f` short. Leave `prepare`, `process`, `split`, `join` and
    /// `take_layout_changed` to the handle.
    pub fn with<R>(&self, f: impl FnOnce(&mut dyn PluginInstance) -> R) -> R {
        let shared = &*self.shared;
        shared.main(|p, state| {
            let p = p.expect("the handle holds the plugin while it lives");
            let r = f(p);
            // A state load can change the latency. A whole instance is still held here, so
            // no node plays it with ports it was not compiled for.
            if state.note_layout_change(p) {
                shared.retire_nodes();
            }
            r
        })
    }

    /// Runs the plugin's main-thread callbacks. Returns true while the graph should be built
    /// and sent again with a fresh [`node`](Self::node): the plugin wants another layout or
    /// latency, or the node asked to play is out of date.
    pub fn pump(&self) -> bool {
        let shared = &*self.shared;
        shared.main(|p, state| {
            if let Some(p) = p {
                p.on_main_thread();
                if state.note_layout_change(p) {
                    shared.retire_nodes();
                }
            }
        });
        self.needs_rebuild()
    }

    /// What [`pump`](Self::pump) returns, without running the callbacks. Always false while
    /// no graph holds a node of this handle.
    pub fn needs_rebuild(&self) -> bool {
        let in_graph = Arc::strong_count(&self.shared) > 1;
        in_graph
            && (self.shared.main(|_, state| state.layout_changed)
                || self.shared.stale.load(Ordering::Relaxed)
                || self.shared.rewire.load(Ordering::Relaxed))
    }

    /// A node for the next graph, with the plugin's current ports and latency. Prepares the
    /// plugin first if it was prepared with other settings or asked for another layout; the
    /// node in the running graph then outputs silence until the new graph is swapped in.
    pub fn node(&self, sample_rate: f64, max_frames: u32) -> PluginNode {
        let (epoch, ports, latency) = self.shared.current(sample_rate, max_frames);
        PluginNode {
            shared: self.shared.clone(),
            id: self.shared.nodes.fetch_add(1, Ordering::Relaxed) + 1,
            epoch,
            ports,
            latency,
            rewired: false,
        }
    }

    /// How many blocks the audio thread output silence for because the main thread was
    /// using the plugin.
    pub fn busy_blocks(&self) -> u64 {
        self.shared.busy.load(Ordering::Relaxed)
    }
}

impl Shared {
    /// Prepares the plugin if it was prepared with other settings or asked for another
    /// layout, and returns the epoch, ports and latency a node for these settings has.
    fn current(&self, sample_rate: f64, max_frames: u32) -> (u64, PortLayout, u32) {
        let due = self
            .main(|_, state| state.layout_changed || state.prepared != (sample_rate, max_frames));
        if due {
            self.prepare(sample_rate, max_frames);
        }
        let (ports, latency) = self.main(|_, state| (state.ports.clone(), state.latency));
        (self.epoch.load(Ordering::Acquire), ports, latency)
    }
}

impl Drop for PluginHandle {
    /// Frees the instance here, on its thread, not wherever the last graph holding a node
    /// is dropped. Those nodes output silence.
    fn drop(&mut self) {
        self.shared.retire_nodes();
        let audio = self
            .shared
            .inner
            .main(|i| std::mem::replace(&mut i.audio, Audio::Gone));
        let instance = self.shared.lock_main().instance.take();
        match (audio, instance) {
            (Audio::Split(half), Some(mut p)) => {
                if let Some(half) = half {
                    p.join(half);
                }
                drop(p);
            }
            (audio, instance) => {
                drop(audio);
                drop(instance);
            }
        }
    }
}

/// A plugin's place in one graph, made by [`PluginHandle::node`]. Its ports and latency are
/// fixed: once the plugin is prepared for a newer layout, or a newer node of the same handle
/// plays, this one outputs silence.
pub struct PluginNode {
    shared: Arc<Shared>,
    id: u64,
    epoch: u64,
    ports: PortLayout,
    latency: u32,
    /// Caught up in `prepare` with other ports than it was made with.
    rewired: bool,
}

impl Drop for PluginNode {
    fn drop(&mut self) {
        // Runs wherever the graph is dropped, so it must not take the plugin.
        let _ = self.shared.live_owner.compare_exchange(
            self.id,
            0,
            Ordering::Relaxed,
            Ordering::Relaxed,
        );
    }
}

impl PluginNode {
    /// Runs `f` on the plugin if this node may play it, retrying briefly while a main-thread
    /// call finishes. Real-time safe: no clock reads or system calls.
    fn on_audio<R>(&mut self, mut f: impl FnMut(Target) -> R) -> Option<R> {
        let (id, epoch, rewired) = (self.id, self.epoch, self.rewired);
        let shared = &*self.shared;
        let mut call = |i: &mut Inner| {
            if id < i.owner && shared.live_owner.load(Ordering::Relaxed) == i.owner {
                // A newer node plays; it, not this one, tells the host about its state.
                return None;
            }
            let stale = shared.epoch.load(Ordering::Acquire) != epoch;
            shared.stale.store(stale, Ordering::Relaxed);
            if stale {
                return None;
            }
            let target = match &mut i.audio {
                Audio::Whole(p) => Target::Whole(p.as_mut()),
                Audio::Split(Some(p)) => Target::Split(p.as_mut()),
                Audio::Split(None) | Audio::Gone => return None,
            };
            if i.owner != id {
                i.owner = id;
                shared.live_owner.store(id, Ordering::Relaxed);
            }
            shared.rewire.store(rewired, Ordering::Relaxed);
            Some(f(target))
        };
        for _ in 0..AUDIO_RETRIES {
            if let Some(r) = self.shared.inner.try_audio(&mut call) {
                return r;
            }
            std::hint::spin_loop();
        }
        self.shared.busy.fetch_add(1, Ordering::Relaxed);
        None
    }
}

impl Node for PluginNode {
    fn ports(&self) -> PortLayout {
        self.ports.clone()
    }

    /// Catches up with the plugin when it was prepared again after this node was made, and
    /// prepares it if the graph is compiled for other settings than the plugin has, or the
    /// plugin asked for another layout. If that changes the node's ports, `pump` asks for a
    /// rebuild so the next graph is wired for them.
    fn prepare(&mut self, sample_rate: f64, max_frames: u32) {
        let (epoch, ports, latency) = self.shared.current(sample_rate, max_frames);
        // The graph was wired for the ports this node was made with.
        self.rewired |= ports != self.ports;
        self.epoch = epoch;
        self.ports = ports;
        self.latency = latency;
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        if self.on_audio(|mut p| p.process(ctx)).is_none() {
            for out in ctx.audio_out.iter_mut() {
                out.fill(0.0);
            }
        }
    }

    fn latency(&self) -> u32 {
        self.latency
    }

    fn reset(&mut self) {
        self.on_audio(|mut p| p.reset());
    }
}
