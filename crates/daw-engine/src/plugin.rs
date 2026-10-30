//! Plugin instances in a graph.
//!
//! A [`PluginInstance`] puts main-thread calls and `process` on one object, and a plugin can
//! ask for a new port layout or latency at any time ([`PluginInstance::take_layout_changed`]).
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
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::{self, ThreadId};

use daw_core::exclusive::Exclusive;
use daw_core::node::{Node, PortLayout, ProcessContext};
use daw_core::plugin::PluginInstance;

/// How many times the audio thread retries a plugin the main thread holds before it outputs
/// silence for that block: a few microseconds, enough for a short main-thread call.
const AUDIO_RETRIES: u32 = 100;

struct Inner {
    /// `None` once the handle is dropped.
    plugin: Option<Box<dyn PluginInstance>>,
    /// Settings of the last `prepare`.
    prepared: (f64, u32),
    /// Ports and latency as of the last `prepare`.
    ports: PortLayout,
    latency: u32,
    /// Bumped on every `prepare`: a node built for an older layout must not call the plugin.
    epoch: u64,
    /// The node that last played. A newer node takes over on its first block; older ones
    /// stay quiet.
    owner: u64,
    /// The plugin asked for another layout and has not been prepared since. Also set while
    /// a prepare runs, so one that panicked is done again.
    layout_changed: bool,
    /// The node asked to play last, if it was the owner or newer, could not: it was made
    /// for an older layout.
    stale: bool,
    /// The node that last played caught up with ports other than the ones it was made, and
    /// wired, with.
    rewire: bool,
}

impl Inner {
    fn prepare(&mut self, sample_rate: f64, max_frames: u32) {
        // Retire every node first, so a prepare that panics halfway leaves none calling the
        // plugin with a layout it was not compiled for.
        self.epoch += 1;
        self.layout_changed = true;
        let Some(p) = self.plugin.as_mut() else {
            return;
        };
        p.prepare(sample_rate, max_frames);
        self.prepared = (sample_rate, max_frames);
        self.ports = p.ports();
        self.latency = p.latency();
        // The prepare answered whatever the plugin had asked for before it.
        p.take_layout_changed();
        self.layout_changed = false;
    }

    /// Picks up a layout change the plugin reported or a caller made through `with`.
    fn note_layout_change(&mut self) {
        let Some(p) = self.plugin.as_mut() else {
            return;
        };
        if p.take_layout_changed() {
            self.layout_changed = true;
        }
        if p.ports() != self.ports {
            // Changed without our prepare: no node was compiled for these ports.
            self.epoch += 1;
            self.ports = p.ports();
            self.layout_changed = true;
        }
        if p.latency() != self.latency {
            // Running nodes can go on playing; compensation follows with the next graph.
            self.latency = p.latency();
            self.layout_changed = true;
        }
    }
}

struct Shared {
    inner: Exclusive<Inner>,
    thread: ThreadId,
    /// Set while the main thread is inside the plugin, to catch re-entry, which would spin
    /// forever.
    main_inside: AtomicBool,
    /// Last node handed out.
    nodes: AtomicU64,
    /// `Inner::owner` while that node lives, 0 once it is dropped: then an older node may
    /// take over.
    live_owner: AtomicU64,
    /// Blocks the audio thread output silence for because the main thread held the plugin.
    busy: AtomicU64,
}

impl Shared {
    fn main<R>(&self, f: impl FnOnce(&mut Inner) -> R) -> R {
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
        self.inner.main(f)
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
    pub fn new(plugin: Box<dyn PluginInstance>, sample_rate: f64, max_frames: u32) -> Self {
        let (ports, latency) = (plugin.ports(), plugin.latency());
        Self {
            shared: Arc::new(Shared {
                inner: Exclusive::new(Inner {
                    plugin: Some(plugin),
                    prepared: (sample_rate, max_frames),
                    ports,
                    latency,
                    epoch: 0,
                    owner: 0,
                    layout_changed: false,
                    stale: false,
                    rewire: false,
                }),
                thread: thread::current().id(),
                main_inside: AtomicBool::new(false),
                nodes: AtomicU64::new(0),
                live_owner: AtomicU64::new(0),
                busy: AtomicU64::new(0),
            }),
            _not_send: PhantomData,
        }
    }

    /// Runs `f` on the plugin, for state, parameters and editors. While it runs, the audio
    /// thread waits a few microseconds, then outputs silence for this plugin, so keep `f`
    /// short. Leave `prepare`, `process` and `take_layout_changed` to the handle.
    pub fn with<R>(&self, f: impl FnOnce(&mut dyn PluginInstance) -> R) -> R {
        self.shared.main(|i| {
            let p = i
                .plugin
                .as_mut()
                .expect("the handle holds the plugin while it lives");
            let r = f(p.as_mut());
            // A state load can change the latency.
            i.note_layout_change();
            r
        })
    }

    /// Runs the plugin's main-thread callbacks. Returns true while the graph should be built
    /// and sent again with a fresh [`node`](Self::node): the plugin wants another layout or
    /// latency, or the node asked to play is out of date.
    pub fn pump(&self) -> bool {
        self.shared.main(|i| {
            if let Some(p) = i.plugin.as_mut() {
                p.on_main_thread();
            }
            i.note_layout_change();
        });
        self.needs_rebuild()
    }

    /// What [`pump`](Self::pump) returns, without running the callbacks. Always false while
    /// no graph holds a node of this handle.
    pub fn needs_rebuild(&self) -> bool {
        let in_graph = Arc::strong_count(&self.shared) > 1;
        in_graph
            && self
                .shared
                .main(|i| i.layout_changed || i.stale || i.rewire)
    }

    /// A node for the next graph, with the plugin's current ports and latency. Prepares the
    /// plugin first if it was prepared with other settings or asked for another layout; the
    /// node in the running graph then outputs silence until the new graph is swapped in.
    pub fn node(&self, sample_rate: f64, max_frames: u32) -> PluginNode {
        let (epoch, ports, latency) = self.shared.main(|i| {
            if i.layout_changed || i.prepared != (sample_rate, max_frames) {
                i.prepare(sample_rate, max_frames);
            }
            (i.epoch, i.ports.clone(), i.latency)
        });
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

impl Drop for PluginHandle {
    /// Frees the instance here, on its thread, not wherever the last graph holding a node
    /// is dropped. Those nodes output silence.
    fn drop(&mut self) {
        let plugin = self.shared.main(|i| {
            i.epoch += 1;
            i.plugin.take()
        });
        drop(plugin);
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
    fn on_audio<R>(&mut self, mut f: impl FnMut(&mut dyn PluginInstance) -> R) -> Option<R> {
        let (id, epoch, rewired) = (self.id, self.epoch, self.rewired);
        let live_owner = &self.shared.live_owner;
        let mut call = |i: &mut Inner| {
            if id < i.owner && live_owner.load(Ordering::Relaxed) == i.owner {
                // A newer node plays; it, not this one, tells the host about its state.
                return None;
            }
            i.stale = i.epoch != epoch;
            let p = i.plugin.as_mut().filter(|_| !i.stale)?;
            if i.owner != id {
                i.owner = id;
                live_owner.store(id, Ordering::Relaxed);
            }
            i.rewire = rewired;
            Some(f(p.as_mut()))
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
        let (epoch, ports, latency) = self.shared.main(|i| {
            if i.layout_changed || i.prepared != (sample_rate, max_frames) {
                i.prepare(sample_rate, max_frames);
            }
            (i.epoch, i.ports.clone(), i.latency)
        });
        // The graph was wired for the ports this node was made with.
        self.rewired |= ports != self.ports;
        self.epoch = epoch;
        self.ports = ports;
        self.latency = latency;
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        if self.on_audio(|p| p.process(ctx)).is_none() {
            for out in ctx.audio_out.iter_mut() {
                out.fill(0.0);
            }
        }
    }

    fn latency(&self) -> u32 {
        self.latency
    }

    fn reset(&mut self) {
        self.on_audio(|p| p.reset());
    }
}
