//! A running LV2 plugin.

use std::sync::Arc;

use daw_core::event::{Event, EventKind, EventList, ParamId};
use daw_core::node::{Node, PortLayout, ProcessContext, TransportState};
use daw_core::plugin::{ParamInfo, PluginDescriptor, PluginError, PluginInstance};
use livi::event::{LV2AtomEventBuilder, LV2AtomSequence};
use livi::{EmptyPortConnections, PortIndex, PortType};

use crate::atom::{self, Urids};
use crate::{Shared, midi, ring, ui};

const LV2_CORE: &str = "http://lv2plug.in/ns/lv2core#";
const PPROPS_NOT_AUTOMATIC: &str = "http://lv2plug.in/ns/ext/port-props#notAutomatic";
const RSZ_MINIMUM_SIZE: &str = "http://lv2plug.in/ns/ext/resize-port#minimumSize";
const MIDI_EVENT: &str = "http://lv2plug.in/ns/ext/midi#MidiEvent";
const ATOM_SUPPORTS: &str = "http://lv2plug.in/ns/ext/atom#supports";
const TIME_POSITION: &str = "http://lv2plug.in/ns/ext/time#Position";
/// Subject of serialized states. Any URI works; this one is never resolved.
const STATE_URI: &str = "urn:daw:lv2-state";

/// Default atom buffer size, as in jalv and Ardour.
const ATOM_CAPACITY: usize = 8192;
/// Parameter changes buffered while waiting for their segment boundary.
const MAX_PENDING_PARAMS: usize = 64;

/// An instantiated LV2 plugin, usable as a graph [`Node`].
pub struct Lv2Instance {
    // Declared first so it is dropped before the plugin it points into.
    editor: Option<ui::Editor>,
    ui_info: Option<ui::UiInfo>,
    /// The UI's ends of the rings while no editor holds them.
    ui_ends: Option<ui::UiEnds>,
    from_ui: rtrb::Consumer<u8>,
    to_ui: rtrb::Producer<u8>,
    ui_active: bool,
    control_snapshot: Vec<(u32, f32)>,
    /// Where messages from the UI are read on the audio thread; as big as the
    /// ring, so any message fits.
    ui_scratch: Vec<u8>,

    inst: livi::Instance,
    shared: Arc<Shared>,
    world: Arc<livi::World>,
    features: Arc<livi::Features>,
    urids: Urids,
    descriptor: PluginDescriptor,
    sample_rate: f64,
    max_frames: u32,

    n_audio_in: usize,
    n_audio_out: usize,
    silence: Vec<f32>,
    scratch_out: Vec<Vec<f32>>,
    cv_in: Vec<Vec<f32>>,
    cv_out: Vec<Vec<f32>>,
    atom_in: Vec<LV2AtomSequence>,
    atom_in_ports: Vec<u32>,
    atom_out: Vec<LV2AtomSequence>,
    atom_out_ports: Vec<u32>,
    atom_out_midi: Vec<bool>,
    midi_in: Option<usize>,
    time_in: Option<usize>,

    params: Vec<ParamInfo>,
    latency_port: Option<usize>,
    /// (playing, bpm, position expected at the next block) last sent.
    last_transport: Option<(bool, f64, i64)>,
    pending_reset: bool,
}

impl Lv2Instance {
    pub(crate) fn new(
        shared: Arc<Shared>,
        descriptor: &PluginDescriptor,
        sample_rate: f64,
        max_frames: u32,
    ) -> Result<Self, PluginError> {
        if !(sample_rate.is_finite() && sample_rate > 0.0) {
            return Err(PluginError::Load(format!("bad sample rate {sample_rate}")));
        }
        // Plugins are always told the host's fixed maximum block; longer host
        // blocks are split in `process`.
        let _ = max_frames;
        let max_frames = crate::HOST_MAX_BLOCK as u32;
        let world = shared.world(&descriptor.path);
        let plugin = world.plugin_by_uri(&descriptor.id).ok_or_else(|| {
            PluginError::Load(format!(
                "{} not found in {} or it needs features this host lacks",
                descriptor.id,
                descriptor.path.display()
            ))
        })?;
        let features = shared.features(&world)?;
        let urids = Urids::new(&features);
        // SAFETY: the plugin comes from lilv's validated metadata, and the
        // sample rate is positive and finite. Port buffers are only connected
        // in `run_segment`, for the duration of each run.
        let inst = unsafe { plugin.instantiate(features.clone(), sample_rate) }
            .map_err(|e| PluginError::Load(format!("{}: {e}", descriptor.id)))?;

        let lilv = world.raw();
        let raw = plugin.raw();
        let midi_event = lilv.new_uri(MIDI_EVENT);
        let time_position = lilv.new_uri(TIME_POSITION);
        let minimum_size = lilv.new_uri(RSZ_MINIMUM_SIZE);
        let atom_capacity = |index: usize| {
            raw.port_by_index(index)
                .and_then(|p| p.get(&minimum_size))
                .and_then(|n| n.as_int())
                .map_or(ATOM_CAPACITY, |n| (n.max(0) as usize).max(ATOM_CAPACITY))
        };
        let supports = |index: usize, ev: &livi::lilv::node::Node| {
            raw.port_by_index(index)
                .is_some_and(|p| p.supports_event(ev))
        };
        let atom_supports = lilv.new_uri(ATOM_SUPPORTS);
        let declares_nothing = |index: usize| {
            raw.port_by_index(index)
                .is_none_or(|p| p.value(&atom_supports).count() == 0)
        };

        let mut atom_in = Vec::new();
        let mut atom_in_ports = Vec::new();
        let mut midi_in = None;
        let mut time_in = None;
        for p in plugin.ports_with_type(PortType::AtomSequenceInput) {
            let i = atom_in.len();
            if midi_in.is_none() && supports(p.index.0, &midi_event) {
                midi_in = Some(i);
            }
            if time_in.is_none() && supports(p.index.0, &time_position) {
                time_in = Some(i);
            }
            atom_in.push(LV2AtomSequence::new(&features, atom_capacity(p.index.0)));
            atom_in_ports.push(p.index.0 as u32);
        }
        // A plugin whose only atom input declares no event types at all still
        // gets MIDI: some plugins leave atom:supports out.
        if midi_in.is_none() && atom_in.len() == 1 && declares_nothing(atom_in_ports[0] as usize) {
            midi_in = Some(0);
        }
        let mut atom_out = Vec::new();
        let mut atom_out_ports = Vec::new();
        let mut atom_out_midi = Vec::new();
        for p in plugin.ports_with_type(PortType::AtomSequenceOutput) {
            atom_out.push(LV2AtomSequence::new(&features, atom_capacity(p.index.0)));
            atom_out_ports.push(p.index.0 as u32);
            atom_out_midi.push(supports(p.index.0, &midi_event));
        }
        if atom_out.len() == 1 && !atom_out_midi[0] && declares_nothing(atom_out_ports[0] as usize)
        {
            atom_out_midi[0] = true;
        }

        let latency_port = raw.latency_port_index();
        let not_automatic = lilv.new_uri(PPROPS_NOT_AUTOMATIC);
        let designation = lilv.new_uri(&format!("{LV2_CORE}designation"));
        let free_wheeling = lilv.new_uri(&format!("{LV2_CORE}freeWheeling"));
        let reports_latency = lilv.new_uri(&format!("{LV2_CORE}reportsLatency"));
        let params = plugin
            .ports_with_type(PortType::ControlInput)
            .filter_map(|p| {
                let lp = raw.port_by_index(p.index.0)?;
                if lp.has_property(&reports_latency)
                    || lp.get(&designation).is_some_and(|d| d == free_wheeling)
                {
                    return None;
                }
                Some(ParamInfo {
                    id: p.index.0 as ParamId,
                    name: p.name.clone(),
                    min: f64::from(p.min_value.unwrap_or(0.0)),
                    max: f64::from(p.max_value.unwrap_or(1.0)),
                    default: f64::from(p.default_value),
                    automatable: !lp.has_property(&not_automatic),
                })
            })
            .collect();

        let counts = plugin.port_counts();
        let frames = max_frames as usize;
        let (to_dsp, from_ui) = ring::channel();
        let (to_ui, from_dsp) = ring::channel();
        let ui_info = ui::UiInfo::find(&world, &plugin);
        let mut descriptor = descriptor.clone();
        descriptor.ports = (counts.audio_inputs as u16, counts.audio_outputs as u16);

        let mut this = Self {
            editor: None,
            ui_info,
            ui_ends: Some(ui::UiEnds { to_dsp, from_dsp }),
            from_ui,
            to_ui,
            ui_active: false,
            control_snapshot: Vec::new(),
            ui_scratch: vec![0; ring::CAPACITY],
            inst,
            shared,
            world: world.clone(),
            features,
            urids,
            descriptor,
            sample_rate,
            max_frames,
            n_audio_in: counts.audio_inputs,
            n_audio_out: counts.audio_outputs,
            silence: vec![0.0; frames],
            scratch_out: vec![vec![0.0; frames]; counts.audio_outputs],
            cv_in: vec![vec![0.0; frames]; counts.cv_inputs],
            cv_out: vec![vec![0.0; frames]; counts.cv_outputs],
            atom_in,
            atom_in_ports,
            atom_out,
            atom_out_ports,
            atom_out_midi,
            midi_in,
            time_in,
            params,
            latency_port,
            last_transport: None,
            pending_reset: false,
        };
        // LV2 only reports latency after a run, so run one silent block.
        if this.latency_port.is_some() {
            this.run_segment(&[], &mut [], None, 0, frames.min(64));
        }
        Ok(this)
    }

    /// Maps a URI to the URID this instance's plugin sees, for building atoms.
    pub fn urid(&self, uri: &str) -> u32 {
        std::ffi::CString::new(uri).map_or(0, |c| self.features.urid(&c))
    }

    /// Queues a complete atom (header and body) for the atom input port with
    /// index `port`, delivered at the start of the next `process`. This is the
    /// path plugin UIs use; it also lets the host send `patch:Set` messages.
    /// Returns false if the queue is full. Not for the audio thread.
    pub fn queue_atom(&mut self, port: u32, atom: &[u8]) -> bool {
        match (&mut self.ui_ends, &mut self.editor) {
            (Some(ends), _) => ring::write(&mut ends.to_dsp, port, ring::ATOM, atom),
            (None, Some(editor)) => editor.queue(port, ring::ATOM, atom),
            (None, None) => false,
        }
    }

    /// Applies what the UI (or `queue_atom`) sent since the last block.
    fn drain_from_ui(&mut self) {
        let buf = &mut self.ui_scratch;
        while let Some(m) = ring::read(&mut self.from_ui, buf) {
            let Some(len) = m.len else { continue };
            match m.kind {
                ring::CONTROL if len == 4 => {
                    let v = f32::from_ne_bytes(buf[..4].try_into().unwrap());
                    self.inst.set_control_input(PortIndex(m.port as usize), v);
                }
                ring::ATOM if len >= 8 => {
                    let Some(i) = self.atom_in_ports.iter().position(|&p| p == m.port) else {
                        continue;
                    };
                    let size = u32::from_ne_bytes(buf[..4].try_into().unwrap()) as usize;
                    let ty = u32::from_ne_bytes(buf[4..8].try_into().unwrap());
                    let body = &buf[8..len.min(8 + size)];
                    atom::push_event(&mut self.atom_in[i], 0, ty, body);
                }
                _ => {}
            }
        }
    }

    fn send_transport(&mut self, t: &TransportState, frames: usize) {
        let Some(i) = self.time_in else { return };
        let pos = t.position.0;
        let changed = match self.last_transport {
            None => true,
            Some((playing, bpm, expected)) => {
                playing != t.playing || bpm != t.bpm || expected != pos
            }
        };
        let next = if t.playing { pos + frames as i64 } else { pos };
        self.last_transport = Some((t.playing, t.bpm, next));
        if !changed {
            return;
        }
        let rate = if t.sample_rate > 0.0 {
            t.sample_rate
        } else {
            self.sample_rate
        };
        let mut buf = [0u8; 192];
        let len = atom::time_position(&self.urids, &mut buf, pos, t.playing, t.bpm, rate);
        if let Ok(ev) = LV2AtomEventBuilder::<192>::new(0, self.urids.atom_object, &buf[..len]) {
            let _ = self.atom_in[i].push_event(&ev);
        }
    }

    fn send_reset(&mut self) {
        self.pending_reset = false;
        self.last_transport = None;
        let Some(i) = self.midi_in else { return };
        for ch in 0..16u8 {
            for cc in [120u8, 123] {
                let _ = self.atom_in[i].push_midi_event::<3>(
                    0,
                    self.urids.midi_event,
                    &[0xb0 | ch, cc, 0],
                );
            }
        }
    }

    /// Runs the plugin over `[start, end)` of the block with whatever is in the
    /// atom inputs, then collects outputs and clears the inputs.
    fn run_segment(
        &mut self,
        audio_in: &[&[f32]],
        audio_out: &mut [&mut [f32]],
        events_out: Option<&mut EventList>,
        start: usize,
        end: usize,
    ) {
        let n = end - start;
        if n == 0 {
            return;
        }
        let host_in = audio_in.len().min(self.n_audio_in);
        let host_out = audio_out.len().min(self.n_audio_out);
        let silence = &self.silence[..n];
        let ins = Exact::new(
            audio_in[..host_in]
                .iter()
                .map(|b| &b[start..end])
                .chain(std::iter::repeat_n(silence, self.n_audio_in - host_in)),
            self.n_audio_in,
        );
        let outs = Exact::new(
            audio_out[..host_out]
                .iter_mut()
                .map(|b| &mut b[start..end])
                .chain(self.scratch_out[host_out..].iter_mut().map(|b| &mut b[..n])),
            self.n_audio_out,
        );
        let ports = EmptyPortConnections::new()
            .with_audio_inputs(ins)
            .with_audio_outputs(outs)
            .with_atom_sequence_inputs(self.atom_in.iter())
            .with_atom_sequence_outputs(self.atom_out.iter_mut())
            .with_cv_inputs(self.cv_in.iter().map(|b| &b[..n]))
            .with_cv_outputs(self.cv_out.iter_mut().map(|b| &mut b[..n]));
        // SAFETY: every buffer handed over lives for this call and has at
        // least `n` frames; atom buffers use the instance's URID map. Errors
        // only report mismatched counts, which the construction above rules
        // out, and there is nothing useful to do with them on this thread.
        let _ = unsafe { self.inst.run(n, ports) };

        let mut events_out = events_out;
        for (j, seq) in self.atom_out.iter().enumerate() {
            for ev in seq.iter() {
                let ty = ev.event.body.type_;
                if ty == self.urids.midi_event
                    && self.atom_out_midi[j]
                    && let (Some(out), Some(kind)) =
                        (events_out.as_deref_mut(), midi::from_midi(ev.data))
                {
                    let frame = (start as i64 + ev.event.time_in_frames.max(0)) as u32;
                    let _ = out.push(Event { frame, kind });
                }
                if self.ui_active {
                    // The atom header, then its body, as one message.
                    let mut header = [0u8; 8];
                    header[..4].copy_from_slice(&(ev.data.len() as u32).to_ne_bytes());
                    header[4..].copy_from_slice(&ty.to_ne_bytes());
                    ring::write_parts(
                        &mut self.to_ui,
                        self.atom_out_ports[j],
                        ring::ATOM,
                        &header,
                        ev.data,
                    );
                }
            }
        }
        for seq in &mut self.atom_in {
            seq.clear();
        }
    }

    /// Processes `[start, end)` of a `frames`-long block, at most `max_frames`
    /// long. Control ports are block-rate in LV2, so the range is split
    /// wherever a parameter changes. A MIDI event at the very frame of a change
    /// stays in the earlier segment, which moves that change one frame later.
    #[allow(unused_assignments)] // the macro's last expansion resets state for nothing
    /// Starts at event index `first` and returns the index of the first event
    /// past the range.
    fn process_range(
        &mut self,
        ctx: &mut ProcessContext,
        start: usize,
        end: usize,
        frames: usize,
        first: usize,
    ) -> usize {
        let mut next = first;
        let mut pending = [(0 as ParamId, 0.0f64); MAX_PENDING_PARAMS];
        let mut n_pending = 0;
        let mut pending_at = start;
        let mut seg_start = start;
        let mut last_midi: Option<usize> = None;
        // Runs up to the pending changes' frame, then applies them in order.
        macro_rules! flush {
            () => {
                if pending_at > seg_start {
                    self.run_segment(
                        ctx.audio_in,
                        ctx.audio_out,
                        Some(&mut *ctx.events_out),
                        seg_start,
                        pending_at,
                    );
                    seg_start = pending_at;
                    last_midi = None;
                }
                for &(p, v) in &pending[..n_pending] {
                    self.apply_param(p, v);
                }
                n_pending = 0;
            };
        }
        for ev in ctx.events_in.iter().skip(first) {
            let f = (ev.frame as usize).min(frames - 1);
            if f >= end {
                break;
            }
            next += 1;
            if n_pending > 0 && f >= pending_at {
                flush!();
            }
            match ev.kind {
                EventKind::ParamValue { param, value } => {
                    let at = f.max(last_midi.map_or(0, |m| m + 1));
                    if at <= seg_start {
                        self.apply_param(param, value);
                        continue;
                    }
                    if n_pending == MAX_PENDING_PARAMS {
                        // Out of room: land the waiting ones first, in order.
                        flush!();
                        if at <= seg_start {
                            self.apply_param(param, value);
                            continue;
                        }
                    }
                    pending[n_pending] = (param, value);
                    n_pending += 1;
                    pending_at = at;
                }
                ref kind => {
                    let (Some(i), Some((bytes, len))) = (self.midi_in, midi::to_midi(kind)) else {
                        continue;
                    };
                    let time = f.saturating_sub(seg_start) as i64;
                    let _ = self.atom_in[i].push_midi_event::<3>(
                        time,
                        self.urids.midi_event,
                        &bytes[..len],
                    );
                    last_midi = Some(f);
                }
            }
        }
        if n_pending > 0 && pending_at < end {
            flush!();
        }
        self.run_segment(
            ctx.audio_in,
            ctx.audio_out,
            Some(&mut *ctx.events_out),
            seg_start,
            end,
        );
        // Changes at or past the end take effect from the next range.
        for &(p, v) in &pending[..n_pending] {
            self.apply_param(p, v);
        }
        next
    }

    fn apply_param(&mut self, param: ParamId, value: f64) {
        self.inst
            .set_control_input(PortIndex(param as usize), value as f32);
    }

    fn control_values(&mut self) {
        self.control_snapshot.clear();
        self.control_snapshot.extend(
            self.inst
                .iter_control_inputs()
                .chain(self.inst.iter_control_outputs())
                .map(|c| (c.port_index.0 as u32, c.value)),
        );
    }
}

impl Node for Lv2Instance {
    fn ports(&self) -> PortLayout {
        PortLayout {
            audio_in: self.n_audio_in as u16,
            audio_out: self.n_audio_out as u16,
            event_in: self.midi_in.is_some() || !self.params.is_empty(),
            event_out: self.atom_out_midi.iter().any(|&m| m),
        }
    }

    /// Any block size works without re-instantiating (see `process`); a new
    /// sample rate needs a new instance, which takes over the old one's state.
    fn prepare(&mut self, sample_rate: f64, max_frames: u32) {
        if sample_rate == self.sample_rate {
            return;
        }
        let state = self.save_state();
        let reopen = self.editor.is_some();
        self.close_editor();
        match Self::new(
            self.shared.clone(),
            &self.descriptor,
            sample_rate,
            max_frames,
        ) {
            Ok(mut fresh) => {
                if let Ok(state) = state
                    && let Err(e) = fresh.load_state(&state)
                {
                    log::warn!(
                        "{}: state lost on re-instantiation: {e}",
                        self.descriptor.id
                    );
                }
                *self = fresh;
            }
            Err(e) => log::error!("{}: re-instantiation failed: {e}", self.descriptor.id),
        }
        if reopen {
            let _ = self.open_editor();
        }
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        let frames = ctx.frames as usize;
        if frames == 0 {
            return;
        }
        self.drain_from_ui();
        self.send_transport(ctx.transport, frames);
        if self.pending_reset {
            self.send_reset();
        }
        // The plugin's maximum block is fixed at instantiation; a longer host
        // block is run in pieces.
        let max = self.max_frames as usize;
        let mut start = 0;
        let mut next_event = 0;
        while start < frames {
            let end = (start + max).min(frames);
            next_event = self.process_range(ctx, start, end, frames, next_event);
            start = end;
        }
    }

    fn latency(&self) -> u32 {
        self.latency_port
            .and_then(|p| self.inst.control_output(PortIndex(p)))
            .map_or(0, |v| v.max(0.0) as u32)
    }

    fn reset(&mut self) {
        self.pending_reset = true;
    }
}

impl PluginInstance for Lv2Instance {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    fn params(&self) -> Vec<ParamInfo> {
        self.params.clone()
    }

    /// The control port's current value, or NaN if `id` is not a control input.
    fn param_value(&self, id: ParamId) -> f64 {
        self.inst
            .control_input(PortIndex(id as usize))
            .map_or(f64::NAN, f64::from)
    }

    /// Control port values plus whatever the plugin stores through the state
    /// extension, as a Turtle document.
    fn save_state(&mut self) -> Result<Vec<u8>, PluginError> {
        let state = self
            .inst
            .state()
            .map_err(|e| PluginError::State(e.to_string()))?;
        let ttl = state
            .to_string(STATE_URI)
            .map_err(|e| PluginError::State(e.to_string()))?;
        Ok(ttl.into_bytes())
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), PluginError> {
        let ttl = std::str::from_utf8(data).map_err(|e| PluginError::State(e.to_string()))?;
        let state = self
            .world
            .state_from_string(&self.features, ttl)
            .map_err(|e| PluginError::State(e.to_string()))?;
        self.inst
            .restore_state(&state)
            .map_err(|e| PluginError::State(e.to_string()))
    }

    fn has_editor(&self) -> bool {
        self.ui_info.is_some()
    }

    fn open_editor(&mut self) -> Result<(), PluginError> {
        if self.editor.is_some() {
            return Ok(());
        }
        let info = self
            .ui_info
            .as_ref()
            .ok_or_else(|| PluginError::Editor("plugin has no supported UI".into()))?;
        let ends = self
            .ui_ends
            .take()
            .ok_or_else(|| PluginError::Editor("UI channels unavailable".into()))?;
        let instance = self.inst.raw().instance();
        let host = ui::HostRefs {
            features: &self.features,
            urids: &self.urids,
            plugin_uri: &self.descriptor.id,
            title: &self.descriptor.name,
            handle: instance.handle(),
            extension_data: instance.descriptor().and_then(|d| d.extension_data),
        };
        match ui::Editor::open(info, host, ends) {
            Ok(editor) => {
                self.editor = Some(editor);
                self.ui_active = true;
                // Give the UI every current value right away.
                self.on_main_thread();
                Ok(())
            }
            Err((e, ends)) => {
                self.ui_ends = Some(ends);
                Err(e)
            }
        }
    }

    fn close_editor(&mut self) {
        self.ui_active = false;
        if let Some(editor) = self.editor.take() {
            self.ui_ends = Some(editor.close());
        }
    }

    /// Plugin workers run on livi's own worker thread, not here, so slow work
    /// (sample loading) never stalls the UI.
    fn on_main_thread(&mut self) {
        if self.editor.is_none() {
            return;
        }
        self.control_values();
        let open = self
            .editor
            .as_mut()
            .is_some_and(|e| e.idle(&self.control_snapshot));
        if !open {
            self.close_editor();
        }
    }
}

impl Drop for Lv2Instance {
    fn drop(&mut self) {
        self.close_editor();
    }
}

/// Adds the `ExactSizeIterator` that `livi` wants to iterator chains whose
/// length we know.
struct Exact<I> {
    inner: I,
    left: usize,
}

impl<I> Exact<I> {
    fn new(inner: I, len: usize) -> Self {
        Self { inner, left: len }
    }
}

impl<I: Iterator> Iterator for Exact<I> {
    type Item = I::Item;

    fn next(&mut self) -> Option<I::Item> {
        let item = self.inner.next()?;
        self.left = self.left.saturating_sub(1);
        Some(item)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.left, Some(self.left))
    }
}

impl<I: Iterator> ExactSizeIterator for Exact<I> {}
