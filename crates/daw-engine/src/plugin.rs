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
//! On the main thread, call [`PluginHandle::pump`] about 60 times a second. When it returns
//! true, build and send the graph again; [`PluginHandle::node`] then prepares the plugin, and
//! the rest of the graph plays on while that one plugin restarts. A plain rebuild with no
//! change does not prepare it again, so it plays on without a gap.
//!
//! Every method of the handle, and compiling a graph that holds one of its nodes, must run on
//! the thread that created the instance: some formats (CLAP) check. Let the controller drop
//! retired graphs on that thread too ([`EngineController::collect_garbage`]), since the last
//! graph that holds a node can be the one that frees the instance.
//!
//! [`EngineController::collect_garbage`]: crate::EngineController::collect_garbage

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use daw_core::exclusive::Exclusive;
use daw_core::node::{Node, PortLayout, ProcessContext};
use daw_core::plugin::PluginInstance;

/// How long the audio thread waits for a main-thread call to finish before it gives up on the
/// block and outputs silence for that plugin. Most main-thread calls take microseconds.
const AUDIO_WAIT: Duration = Duration::from_micros(100);

struct Inner {
    plugin: Box<dyn PluginInstance>,
    /// Settings of the last `prepare`.
    prepared: Option<(f64, u32)>,
    /// Bumped on every `prepare`: a node built for an older layout must not call the plugin.
    epoch: u64,
    /// The plugin asked for another layout and has not been prepared since.
    layout_changed: bool,
}

impl Inner {
    fn prepare(&mut self, sample_rate: f64, max_frames: u32) {
        self.plugin.prepare(sample_rate, max_frames);
        self.prepared = Some((sample_rate, max_frames));
        self.epoch += 1;
        // The prepare answered whatever the plugin had asked for before it.
        self.plugin.take_layout_changed();
        self.layout_changed = false;
    }

    fn note_layout_change(&mut self) {
        if self.plugin.take_layout_changed() {
            self.layout_changed = true;
        }
    }
}

struct Shared {
    inner: Exclusive<Inner>,
    /// Blocks the audio thread output silence for because the main thread held the plugin.
    busy: AtomicU64,
}

/// Owns a plugin instance on the main thread. See the [module docs](self).
pub struct PluginHandle {
    shared: Arc<Shared>,
}

impl PluginHandle {
    pub fn new(plugin: Box<dyn PluginInstance>) -> Self {
        Self {
            shared: Arc::new(Shared {
                inner: Exclusive::new(Inner {
                    plugin,
                    prepared: None,
                    epoch: 0,
                    layout_changed: false,
                }),
                busy: AtomicU64::new(0),
            }),
        }
    }

    /// Runs `f` on the plugin, for state, parameters and editors. While it runs, the audio
    /// thread waits up to 100 µs, then outputs silence for this plugin, so keep `f` short.
    pub fn with<R>(&self, f: impl FnOnce(&mut dyn PluginInstance) -> R) -> R {
        self.shared.inner.main(|i| {
            let r = f(i.plugin.as_mut());
            // A state load can change the latency.
            i.note_layout_change();
            r
        })
    }

    /// Runs the plugin's main-thread callbacks. Returns true while the plugin wants another
    /// layout or latency: build and send the graph again with a fresh [`node`](Self::node).
    pub fn pump(&self) -> bool {
        self.shared.inner.main(|i| {
            i.plugin.on_main_thread();
            i.note_layout_change();
            i.layout_changed
        })
    }

    /// Whether the plugin wants another layout or latency, as found by [`pump`](Self::pump)
    /// or [`with`](Self::with).
    pub fn layout_changed(&self) -> bool {
        self.shared.inner.main(|i| i.layout_changed)
    }

    /// A node for the next graph, with the plugin's current ports and latency. Prepares the
    /// plugin first if it has never been prepared with these settings or it asked for another
    /// layout; the node in the running graph then outputs silence until the new graph is
    /// swapped in.
    pub fn node(&self, sample_rate: f64, max_frames: u32) -> PluginNode {
        let (epoch, ports, latency) = self.shared.inner.main(|i| {
            if i.layout_changed || i.prepared != Some((sample_rate, max_frames)) {
                i.prepare(sample_rate, max_frames);
            }
            (i.epoch, i.plugin.ports(), i.plugin.latency())
        });
        PluginNode {
            shared: self.shared.clone(),
            epoch,
            ports,
            latency,
            prepared: (sample_rate, max_frames),
        }
    }

    /// How many blocks the audio thread output silence for because the main thread was
    /// using the plugin.
    pub fn busy_blocks(&self) -> u64 {
        self.shared.busy.load(Ordering::Relaxed)
    }
}

/// A plugin's place in one graph, made by [`PluginHandle::node`]. Its ports and latency are
/// fixed: once the plugin is prepared for a newer graph, this one outputs silence.
pub struct PluginNode {
    shared: Arc<Shared>,
    epoch: u64,
    ports: PortLayout,
    latency: u32,
    prepared: (f64, u32),
}

impl PluginNode {
    /// Runs `f` on the plugin if it is still prepared for this node, waiting briefly for a
    /// main-thread call to finish. Real-time safe.
    fn on_audio<R>(&mut self, mut f: impl FnMut(&mut dyn PluginInstance) -> R) -> Option<R> {
        let epoch = self.epoch;
        let mut call = |i: &mut Inner| (i.epoch == epoch).then(|| f(i.plugin.as_mut()));
        if let Some(r) = self.shared.inner.try_audio(&mut call) {
            return r;
        }
        let deadline = Instant::now() + AUDIO_WAIT;
        loop {
            std::hint::spin_loop();
            if let Some(r) = self.shared.inner.try_audio(&mut call) {
                return r;
            }
            if Instant::now() >= deadline {
                self.shared.busy.fetch_add(1, Ordering::Relaxed);
                return None;
            }
        }
    }
}

impl Node for PluginNode {
    fn ports(&self) -> PortLayout {
        self.ports.clone()
    }

    /// The node was prepared by [`PluginHandle::node`]. Compiling it for other settings
    /// prepares the plugin again, which retires every other node of this plugin.
    fn prepare(&mut self, sample_rate: f64, max_frames: u32) {
        if self.prepared == (sample_rate, max_frames) {
            return;
        }
        let (epoch, ports, latency) = self.shared.inner.main(|i| {
            i.prepare(sample_rate, max_frames);
            (i.epoch, i.plugin.ports(), i.plugin.latency())
        });
        self.epoch = epoch;
        self.ports = ports;
        self.latency = latency;
        self.prepared = (sample_rate, max_frames);
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
