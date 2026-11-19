//! A running LV2 plugin.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use daw_core::event::{Event, EventKind, EventList, ParamId, midi};
use daw_core::node::{Node, PortLayout, ProcessContext, TransportState};
use daw_core::plugin::{
    ParamInfo, PluginDescriptor, PluginError, PluginInstance, PluginProcessor, Steps,
};
use daw_core::split::{SharedAudio, SplitAudio};
use daw_plugin_window::EditorTitle;
use livi::event::{LV2AtomEventBuilder, LV2AtomSequence};
use livi::{EmptyPortConnections, PortIndex, PortType};

use crate::atom::{self, Urids};
use crate::{Shared, ring, ui};

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

/// An instantiated LV2 plugin, usable as a graph [`Node`].
///
/// The plugin itself and everything `process` touches is its [`Audio`], which
/// [`split`](PluginInstance::split) lends to the audio thread. The UI and the
/// DSP talk through rings and [`Published`], so an open editor never makes a
/// block wait; only saving and loading state take the plugin from the audio
/// thread, as the LV2 state extension requires.
pub struct Lv2Instance {
    // Declared first so it is dropped before the plugin it points into.
    editor: Option<ui::Editor>,
    editor_title: EditorTitle,
    /// The UI changed a value, and `take_state_changed` has not said so yet.
    state_changed: bool,
    ui_info: Option<ui::UiInfo>,
    /// The UI's ends of the rings while no editor holds them.
    ui_ends: Option<ui::UiEnds>,
    control_snapshot: Vec<(u32, f32)>,

    audio: SharedAudio<Audio>,
    published: Arc<Published>,
    shared: Arc<Shared>,
    world: Arc<livi::World>,
    features: Arc<livi::Features>,
    urids: Urids,
    descriptor: PluginDescriptor,
    sample_rate: f64,
    layout: PortLayout,
    params: Vec<ParamInfo>,
    latency_port: Option<usize>,
    /// Port index and capacity of every atom input, for `queue_atom`.
    atom_inputs: Vec<(u32, usize)>,
}

/// What the audio side tells the main thread without either waiting for the
/// other: the control port values after each block, and whether a UI listens.
struct Published {
    /// Port index and value (as `f32` bits) of every control input, then of
    /// every control output.
    controls: Vec<(u32, AtomicU32)>,
    control_inputs: usize,
    ui_active: AtomicBool,
}

impl Published {
    fn new(inst: &livi::Instance) -> Self {
        Self {
            controls: inst
                .iter_control_inputs()
                .chain(inst.iter_control_outputs())
                .map(|c| (c.port_index.0 as u32, AtomicU32::new(c.value.to_bits())))
                .collect(),
            control_inputs: inst.iter_control_inputs().count(),
            ui_active: AtomicBool::new(false),
        }
    }

    fn store(&self, inst: &livi::Instance) {
        let ports = inst
            .iter_control_inputs()
            .chain(inst.iter_control_outputs());
        for ((_, slot), c) in self.controls.iter().zip(ports) {
            slot.store(c.value.to_bits(), Ordering::Relaxed);
        }
    }

    fn values(&self) -> impl Iterator<Item = (u32, f32)> + '_ {
        self.controls
            .iter()
            .map(|(port, v)| (*port, f32::from_bits(v.load(Ordering::Relaxed))))
    }

    fn input(&self, port: u32) -> Option<f32> {
        self.values()
            .take(self.control_inputs)
            .find_map(|(p, v)| (p == port).then_some(v))
    }

    fn output(&self, port: u32) -> Option<f32> {
        self.values()
            .skip(self.control_inputs)
            .find_map(|(p, v)| (p == port).then_some(v))
    }
}

/// The plugin and what `process` needs to run it. Real-time safe once made.
struct Audio {
    inst: livi::Instance,
    urids: Urids,
    sample_rate: f64,
    max_frames: u32,
    published: Arc<Published>,
    from_ui: rtrb::Consumer<u8>,
    to_ui: rtrb::Producer<u8>,
    /// Where messages from the UI are read; as big as the ring, so any
    /// message fits.
    ui_scratch: Vec<u8>,

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

    /// The control inputs the host knows as parameters.
    param_ids: Vec<ParamId>,
    /// Parameter changes waiting for their segment boundary; preallocated.
    pending: Vec<(ParamId, f64)>,
    /// What the plugin expects at the next block from the last position sent.
    last_transport: Option<Expected>,
    pending_reset: bool,
}

impl Lv2Instance {
    pub(crate) fn new(
        shared: Arc<Shared>,
        descriptor: &PluginDescriptor,
        sample_rate: f64,
    ) -> Result<Self, PluginError> {
        if !(sample_rate.is_finite() && sample_rate > 0.0) {
            return Err(PluginError::Load(format!("bad sample rate {sample_rate}")));
        }
        // Plugins are always told the host's fixed maximum block, whatever the
        // host's block size; longer host blocks are split in `process`.
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
        // livi clamps every control write to the port's bounds, and clamping
        // panics when they are inverted or NaN: on the audio thread, or while
        // restoring a project. Such metadata is broken, so refuse the plugin.
        if let Some(c) = inst
            .iter_control_inputs()
            .find(|c| c.minimum.partial_cmp(&c.maximum).is_none_or(|o| o.is_gt()))
        {
            let name = plugin
                .ports()
                .find(|p| p.index == c.port_index)
                .map_or_else(|| c.port_index.0.to_string(), |p| p.name);
            return Err(PluginError::Load(format!(
                "{}: control port {name} has an invalid range {}..{}",
                descriptor.id, c.minimum, c.maximum
            )));
        }

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
            // Twice the size the plugin asks for: UI atoms may use half (see
            // `ui_atom_room`), host events always have the rest.
            atom_in.push(LV2AtomSequence::new(
                &features,
                2 * atom_capacity(p.index.0),
            ));
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
        let toggled = lilv.new_uri(&format!("{LV2_CORE}toggled"));
        let integer = lilv.new_uri(&format!("{LV2_CORE}integer"));
        let enumeration = lilv.new_uri(&format!("{LV2_CORE}enumeration"));
        let params: Vec<ParamInfo> = plugin
            .ports_with_type(PortType::ControlInput)
            .filter_map(|p| {
                // lv2:sampleRate ports declare fractions of the rate; livi's
                // own bounds for the port are scaled the same way.
                let p = p.scaled(sample_rate);
                let lp = raw.port_by_index(p.index.0)?;
                if lp.has_property(&reports_latency)
                    || lp.get(&designation).is_some_and(|d| d == free_wheeling)
                {
                    return None;
                }
                // Unbounded ports get a range that at least holds the
                // default (scaling can also overflow a huge bound).
                let min = f64::from(
                    p.min_value
                        .filter(|v| v.is_finite())
                        .unwrap_or(p.default_value.min(0.0)),
                );
                let max = f64::from(
                    p.max_value
                        .filter(|v| v.is_finite())
                        .unwrap_or(p.default_value.max(1.0)),
                );
                // A toggle is off or on, its minimum or maximum. An
                // enumeration's scale points are taken as the whole numbers
                // they nearly always are.
                let steps = if lp.has_property(&toggled) {
                    Some(Steps(1))
                } else if lp.has_property(&integer) || lp.has_property(&enumeration) {
                    Steps::whole(min, max)
                } else {
                    None
                };
                Some(ParamInfo {
                    id: p.index.0 as ParamId,
                    name: p.name.clone(),
                    min,
                    max,
                    default: f64::from(p.default_value),
                    automatable: !lp.has_property(&not_automatic),
                    steps,
                })
            })
            .collect();

        let counts = plugin.port_counts();
        let frames = max_frames as usize;
        let (to_dsp, from_ui) = ring::channel();
        // Room for two blocks' worth of every atom output, for the UI.
        let out_bytes: usize = atom_out.iter().map(|s| s.capacity() + 9).sum();
        let (to_ui, from_dsp) = ring::channel_with(2 * out_bytes);
        let ui_info = ui::UiInfo::find(&world, &plugin);
        let mut descriptor = descriptor.clone();
        descriptor.ports = (counts.audio_inputs as u16, counts.audio_outputs as u16);

        let layout = PortLayout {
            audio_in: counts.audio_inputs as u16,
            audio_out: counts.audio_outputs as u16,
            event_in: midi_in.is_some() || !params.is_empty(),
            // Parameter changes made in the plugin's UI go out as events.
            event_out: atom_out_midi.iter().any(|&m| m) || !params.is_empty(),
        };
        let atom_inputs = atom_in_ports
            .iter()
            .zip(&atom_in)
            .map(|(&port, seq)| (port, seq.capacity()))
            .collect();
        let published = Arc::new(Published::new(&inst));
        let mut audio = Audio {
            inst,
            urids,
            sample_rate,
            max_frames,
            published: published.clone(),
            from_ui,
            to_ui,
            ui_scratch: vec![0; ring::CAPACITY],
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
            param_ids: params.iter().map(|p| p.id).collect(),
            pending: Vec::with_capacity(counts.control_inputs),
            last_transport: None,
            pending_reset: false,
        };
        // LV2 only reports latency after a run, so run one silent block.
        if latency_port.is_some() {
            audio.run_segment(&[], &mut [], None, 0, frames.min(64));
            published.store(&audio.inst);
        }
        Ok(Self {
            editor: None,
            editor_title: EditorTitle::default(),
            state_changed: false,
            ui_info,
            ui_ends: Some(ui::UiEnds { to_dsp, from_dsp }),
            control_snapshot: Vec::new(),
            audio: SharedAudio::new(audio),
            published,
            shared,
            world: world.clone(),
            features,
            urids,
            descriptor,
            sample_rate,
            layout,
            params,
            latency_port,
            atom_inputs,
        })
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
        let fits = self.atom_inputs.iter().any(|&(p, capacity)| {
            p == port && atom.len() >= 8 && ui_atom_room(capacity, 0, atom.len()) != Room::Never
        });
        if !fits {
            return false;
        }
        match (&mut self.ui_ends, &mut self.editor) {
            (Some(ends), _) => ring::write(&mut ends.to_dsp, port, ring::ATOM, atom),
            (None, Some(editor)) => editor.queue(port, ring::ATOM, atom),
            (None, None) => false,
        }
    }

    /// Replaces the plugin with one made for `sample_rate` that carries this
    /// one's state. On any failure this instance is left exactly as it was.
    fn reinstantiate(&mut self, sample_rate: f64) -> Result<(), PluginError> {
        let state = self.save_state()?;
        let mut fresh = Self::new(self.shared.clone(), &self.descriptor, sample_rate)?;
        fresh.load_state(&state)?;
        let reopen = self.editor.is_some();
        // Also collects an edit the editor has not reported yet.
        self.close_editor();
        fresh.editor_title = std::mem::take(&mut self.editor_title);
        fresh.state_changed = self.state_changed;
        *self = fresh;
        if reopen {
            let _ = self.open_editor();
        }
        Ok(())
    }

    fn control_values(&mut self) {
        self.control_snapshot.clear();
        self.control_snapshot.extend(self.published.values());
    }
}

impl Audio {
    /// Applies what the UI (or `queue_atom`) sent since the last block. UI
    /// writes to parameters are also reported in `events_out`, as changes the
    /// plugin made, so the host's view of its parameters stays current.
    fn drain_from_ui(&mut self, events_out: &mut EventList) {
        let buf = &mut self.ui_scratch;
        while let Some((port, kind, size)) = ring::peek(&mut self.from_ui) {
            let atom_port = if kind == ring::ATOM {
                atom_in_index(&self.atom_in_ports, port)
            } else {
                None
            };
            let room = atom_port.map(|i| {
                let seq = &self.atom_in[i];
                ui_atom_room(seq.capacity(), seq.size(), size)
            });
            if room == Some(Room::Later) {
                // Leave it (and what follows) for the next block.
                break;
            }
            let Some(m) = ring::read(&mut self.from_ui, buf) else {
                break;
            };
            if room == Some(Room::Never) {
                // Could only fit by crowding out host events: dropped.
                continue;
            }
            let Some(len) = m.len else { continue };
            match m.kind {
                ring::CONTROL if len == 4 => {
                    let v = f32::from_ne_bytes(buf[..4].try_into().unwrap());
                    let set = self.inst.set_control_input(PortIndex(m.port as usize), v);
                    let param = m.port as ParamId;
                    if let Some(value) = set
                        && self.param_ids.contains(&param)
                    {
                        let kind = EventKind::ParamValue {
                            param,
                            value: f64::from(value),
                        };
                        let _ = events_out.push(Event { frame: 0, kind });
                    }
                }
                ring::ATOM if len >= 8 => {
                    let Some(i) = atom_port else {
                        continue;
                    };
                    let size = u32::from_ne_bytes(buf[..4].try_into().unwrap()) as usize;
                    let ty = u32::from_ne_bytes(buf[4..8].try_into().unwrap());
                    let body = &buf[8..len.min(8 + size)];
                    // Room was checked above.
                    atom::push_event(&mut self.atom_in[i], 0, ty, body);
                }
                _ => {}
            }
        }
    }

    fn send_transport(&mut self, t: &TransportState, frames: usize) {
        let Some(i) = self.time_in else { return };
        let t = t.or_default_tempo(self.sample_rate);
        let changed = match self.last_transport {
            None => true,
            Some(expected) => !expected.matches(&t),
        };
        if changed {
            let mut buf = [0u8; 192];
            let len = atom::time_position(&self.urids, &mut buf, &t);
            let sent = LV2AtomEventBuilder::<192>::new(0, self.urids.atom_object, &buf[..len])
                .is_ok_and(|ev| self.atom_in[i].push_event(&ev).is_ok());
            if !sent {
                // The plugin did not hear of this position: try again next block.
                self.last_transport = None;
                return;
            }
        }
        self.last_transport = Some(Expected::after(&t, frames));
    }

    /// Adds a MIDI message `time` frames into the segment being collected in
    /// atom input `i`. Returns false if the input is full.
    fn push_midi(&mut self, i: usize, time: usize, bytes: &[u8]) -> bool {
        self.atom_in[i]
            .push_midi_event::<3>(time as i64, self.urids.midi_event, bytes)
            .is_ok()
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
            for seq in &mut self.atom_in {
                seq.clear();
            }
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
            for ev in atom::output_events(seq, self.urids.atom_sequence) {
                let ty = ev.ty;
                if ty == self.urids.midi_event
                    && self.atom_out_midi[j]
                    && let (Some(out), Some(kind)) =
                        (events_out.as_deref_mut(), midi::decode(ev.data))
                {
                    let time = ev.time.clamp(0, (n - 1) as i64);
                    let frame = (start as i64 + time) as u32;
                    let _ = out.push(Event { frame, kind });
                }
                if self.published.ui_active.load(Ordering::Relaxed) {
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
        // Taken out of `self` (no allocation) so `self` stays free to borrow;
        // its capacity is the number of control inputs, and changes are
        // coalesced per port, so pushes never allocate.
        let mut pending = std::mem::take(&mut self.pending);
        pending.clear();
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
                for &(p, v) in &pending {
                    self.apply_param(p, v);
                }
                pending.clear();
            };
        }
        for ev in ctx.events_in.iter().skip(first) {
            let f = (ev.frame as usize).min(frames - 1);
            if f >= end {
                break;
            }
            next += 1;
            if !pending.is_empty() && f >= pending_at {
                flush!();
            }
            match ev.kind {
                EventKind::ParamValue { param, value } => {
                    if self.inst.control_input(PortIndex(param as usize)).is_none() {
                        continue;
                    }
                    let at = f.max(last_midi.map_or(0, |m| m + 1));
                    if at <= seg_start {
                        self.apply_param(param, value);
                        continue;
                    }
                    // All pending changes share one frame, so a later value for
                    // the same port simply replaces the earlier one.
                    if let Some(slot) = pending.iter_mut().find(|(p, _)| *p == param) {
                        slot.1 = value;
                        continue;
                    }
                    pending.push((param, value));
                    pending_at = at;
                }
                ref kind => {
                    let (Some(i), Some((bytes, len))) = (self.midi_in, midi::encode(kind)) else {
                        continue;
                    };
                    let bytes = &bytes[..len];
                    if !self.push_midi(i, f.saturating_sub(seg_start), bytes) {
                        // The atom input is full: run what it holds up to this
                        // event (at least one frame), which empties it.
                        // `seg_start < end` here, so `at <= end`.
                        let at = f.max(seg_start + 1);
                        self.run_segment(
                            ctx.audio_in,
                            ctx.audio_out,
                            Some(&mut *ctx.events_out),
                            seg_start,
                            at,
                        );
                        seg_start = at;
                        if at == end {
                            // No frame of this range is left to run it in: this
                            // event and the rest wait for the next range. At the
                            // end of the block they are lost, as more arrived
                            // for one frame than the plugin's input holds.
                            next -= 1;
                            break;
                        }
                        self.push_midi(i, 0, bytes);
                    }
                    last_midi = Some(f);
                }
            }
        }
        if !pending.is_empty() && pending_at < end {
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
        for &(p, v) in &pending {
            self.apply_param(p, v);
        }
        pending.clear();
        self.pending = pending;
        next
    }

    fn apply_param(&mut self, param: ParamId, value: f64) {
        self.inst
            .set_control_input(PortIndex(param as usize), value as f32);
    }

    fn process_block(&mut self, ctx: &mut ProcessContext) {
        let frames = ctx.frames as usize;
        if frames == 0 {
            return;
        }
        self.drain_from_ui(ctx.events_out);
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
        self.published.store(&self.inst);
    }
}

impl SplitAudio for Audio {
    fn process(&mut self, ctx: &mut ProcessContext) {
        self.process_block(ctx);
    }

    /// Sent to the plugin as MIDI at the start of the next block.
    fn reset(&mut self) {
        self.pending_reset = true;
    }
}

impl Node for Lv2Instance {
    fn ports(&self) -> PortLayout {
        self.layout.clone()
    }

    /// Any block size works without re-instantiating (see `process`); a new
    /// sample rate needs a new instance, which takes over the old one's state.
    fn prepare(&mut self, sample_rate: f64, _max_frames: u32) {
        if sample_rate == self.sample_rate {
            return;
        }
        if let Err(e) = self.reinstantiate(sample_rate) {
            // Wrong pitch and timing can be heard and undone; settings
            // replaced by defaults would be saved with the project.
            log::error!(
                "{}: stays at {} Hz, could not be moved to {sample_rate} Hz with its state: {e}",
                self.descriptor.id,
                self.sample_rate
            );
        }
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        self.audio.process(ctx);
    }

    fn latency(&self) -> u32 {
        self.latency_port
            .and_then(|p| self.published.output(p as u32))
            .map_or(0, |v| v.max(0.0) as u32)
    }

    fn reset(&mut self) {
        self.audio.reset();
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
        self.published.input(id).map_or(f64::NAN, f64::from)
    }

    /// Control port values plus whatever the plugin stores through the state
    /// extension, as a Turtle document.
    fn save_state(&mut self) -> Result<Vec<u8>, PluginError> {
        // The state extension keeps `save` apart from `run`.
        let state = self
            .audio
            .main(|a| a.inst.state())
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
        let published = &self.published;
        self.audio
            .main(|a| {
                let restored = a.inst.restore_state(&state);
                published.store(&a.inst);
                restored
            })
            .map_err(|e| PluginError::State(e.to_string()))?;
        // The UI hears the loaded values as new ones; what it writes back is no edit.
        if let Some(editor) = &mut self.editor {
            editor.take_state_changed();
        }
        self.state_changed = false;
        Ok(())
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
        let mut ends = self
            .ui_ends
            .take()
            .ok_or_else(|| PluginError::Editor("UI channels unavailable".into()))?;
        // Whatever the plugin sent for an earlier editor is stale now.
        while ring::read(&mut ends.from_dsp, &mut []).is_some() {}
        // For the instance-access and data-access features.
        let (handle, extension_data) = self.audio.main(|a| {
            let instance = a.inst.raw().instance();
            (
                instance.handle(),
                instance.descriptor().and_then(|d| d.extension_data),
            )
        });
        let host = ui::HostRefs {
            features: &self.features,
            urids: &self.urids,
            plugin_uri: &self.descriptor.id,
            title: self.editor_title.or_name(&self.descriptor.name),
            handle,
            extension_data,
        };
        // Before the UI exists: the audio thread plays on meanwhile, and what the plugin
        // answers to a request the UI makes while it is being built must be kept for it.
        self.published.ui_active.store(true, Ordering::Relaxed);
        match ui::Editor::open(info, host, ends) {
            Ok(editor) => {
                self.editor = Some(editor);
                // Give the UI every current value right away.
                self.on_main_thread();
                Ok(())
            }
            Err((e, ends)) => {
                self.published.ui_active.store(false, Ordering::Relaxed);
                self.ui_ends = ends;
                Err(e)
            }
        }
    }

    fn close_editor(&mut self) {
        self.published.ui_active.store(false, Ordering::Relaxed);
        if let Some(mut editor) = self.editor.take() {
            self.state_changed |= editor.take_state_changed();
            self.ui_ends = editor.close();
        }
    }

    fn is_editor_open(&self) -> bool {
        self.editor.is_some()
    }

    fn set_editor_title(&mut self, title: &str) {
        if self.editor_title.set(title)
            && let Some(editor) = &mut self.editor
        {
            editor.set_title(title);
        }
    }

    fn split(&mut self) -> Option<Box<dyn PluginProcessor>> {
        self.audio.lend()
    }

    fn join(&mut self, processor: Box<dyn PluginProcessor>) {
        if !self.audio.take_back(processor) {
            log::warn!(
                "{}: asked to join a processor that is not its own",
                self.descriptor.id
            );
        }
    }

    fn take_state_changed(&mut self) -> bool {
        let ui = self
            .editor
            .as_mut()
            .is_some_and(ui::Editor::take_state_changed);
        std::mem::take(&mut self.state_changed) | ui
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Room {
    Now,
    Later,
    Never,
}

fn atom_in_index(ports: &[u32], port: u32) -> Option<usize> {
    ports.iter().position(|&p| p == port)
}

/// Whether a UI atom of `atom_len` bytes (header included) fits now in an atom
/// input of `capacity` that holds `used` bytes. UI atoms may fill only half of
/// an atom input, so host MIDI, transport and reset events pushed after them
/// always have room.
fn ui_atom_room(capacity: usize, used: usize, atom_len: usize) -> Room {
    let empty = std::mem::size_of::<lv2_raw::LV2AtomSequence>();
    let half = (capacity + empty) / 2;
    let needed = atom::event_size(atom_len.saturating_sub(8));
    if empty + needed > half {
        Room::Never
    } else if used + needed > half {
        Room::Later
    } else {
        Room::Now
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
/// The transport a plugin extrapolates from the last `time:Position` it got.
/// A new position is sent only when the real one differs.
#[derive(Copy, Clone, Debug)]
struct Expected {
    playing: bool,
    bpm: f64,
    position: i64,
    beats: f64,
    numerator: u8,
    denominator: u8,
}

impl Expected {
    /// Where `t` will be `frames` samples on, at its tempo.
    fn after(t: &TransportState, frames: usize) -> Self {
        let next = t.advanced(frames as u32);
        Self {
            playing: t.playing,
            bpm: t.bpm,
            position: next.position.0,
            beats: next.music.beats,
            numerator: t.music.numerator,
            denominator: t.music.denominator,
        }
    }

    fn matches(&self, t: &TransportState) -> bool {
        // A plugin rounds beats from float anyway; this only catches jumps
        // (a locate, a loop, an edited tempo map).
        const BEATS: f64 = 1e-6;
        self.playing == t.playing
            && self.bpm == t.bpm
            && self.position == t.position.0
            && (self.beats - t.music.beats).abs() < BEATS
            && self.numerator == t.music.numerator
            && self.denominator == t.music.denominator
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use daw_core::plugin::PluginFormat;

    /// Plugin `id` of the installed bundle `bundle`, or `None` (a skipped
    /// test) where the bundle is not installed and not required.
    fn installed(bundle: &str, id: &str) -> Option<Lv2Instance> {
        let format = crate::Lv2Format::new();
        let dir = daw_test_support::lv2::find_bundles(format.default_search_paths(), &[bundle])?;
        let found = format
            .scan(&dir.join(bundle))
            .ok()
            .and_then(|scan| scan.plugins.into_iter().find(|d| d.id == id));
        let Some(d) = found else {
            assert!(
                std::env::var_os(daw_test_support::lv2::REQUIRE_ENV).is_none(),
                "{bundle} must contain {id}"
            );
            eprintln!("skipped: {bundle} has no {id}");
            return None;
        };
        Some(format.instantiate_lv2(&d, 48_000.0).unwrap())
    }

    const MDA_DELAY: &str = "http://drobilla.net/plugins/mda/Delay";

    fn mda_delay() -> Option<Lv2Instance> {
        installed("mda.lv2", MDA_DELAY)
    }

    /// Sets a control input as a parameter event at frame 0 would.
    fn set(p: &Lv2Instance, param: ParamId, value: f64) {
        p.audio.main(|a| {
            a.apply_param(param, value);
            a.published.store(&a.inst);
        });
    }

    /// The audio half plays while the instance answers the main thread: it
    /// reads parameters without the plugin, and takes it only for its state.
    #[test]
    fn a_split_instance_serves_the_main_thread_while_its_half_plays() {
        let Some(mut p) = mda_delay() else { return };
        let fb = p.params.iter().find(|i| i.name == "Feedback").unwrap().id;
        let mut half = p.split().expect("LV2 instances split");
        assert!(p.split().is_none());

        let frames = 64;
        let mut outs = vec![vec![0.0f32; frames]; 2];
        let mut block = |half: &mut dyn PluginProcessor, value: f64| {
            let mut out_refs: Vec<&mut [f32]> = outs.iter_mut().map(|b| &mut b[..]).collect();
            let mut events_in = EventList::with_capacity(1);
            let kind = EventKind::ParamValue { param: fb, value };
            events_in.push(Event { frame: 0, kind }).unwrap();
            half.process(&mut ProcessContext {
                frames: frames as u32,
                transport: &TransportState::default(),
                audio_in: &[],
                audio_out: &mut out_refs,
                events_in: &events_in,
                events_out: &mut EventList::with_capacity(8),
            });
        };
        block(&mut *half, 0.25);
        assert_eq!(p.param_value(fb), 0.25);
        let saved = p.save_state().unwrap();
        block(&mut *half, 0.5);
        assert_eq!(p.param_value(fb), 0.5);
        p.load_state(&saved).unwrap();
        assert_eq!(p.param_value(fb), 0.25);

        p.join(half);
        assert!(p.split().is_some());
    }

    /// A plugin whose state cannot be carried to a new sample rate keeps
    /// running as it is, rather than being replaced by one with default
    /// settings that the next save would store.
    #[test]
    fn a_new_sample_rate_never_costs_the_plugin_its_state() {
        let Some(mut p) = mda_delay() else { return };
        let fb = p.params.iter().find(|i| i.name == "Feedback").unwrap().id;
        set(&p, fb, 0.25);
        p.state_changed = true;

        // The state of a Delay does not load into another plugin.
        p.descriptor.id = "http://drobilla.net/plugins/mda/Ambience".into();
        p.prepare(44_100.0, 512);
        assert_eq!(p.sample_rate, 48_000.0);
        assert_eq!(p.ports().audio_in, 2);
        assert_eq!(p.param_value(fb), 0.25);

        p.descriptor.id = MDA_DELAY.into();
        p.prepare(44_100.0, 512);
        assert_eq!(p.sample_rate, 44_100.0);
        assert_eq!(p.param_value(fb), 0.25);
        // An edit not yet reported to the host survives the move.
        assert!(p.take_state_changed());
    }

    /// More MIDI for one frame than the atom input holds, several times over,
    /// at the last frame of a block and in a block of one frame: there is no
    /// frame left to run the surplus in, which must cost only those events.
    #[test]
    fn a_flood_of_midi_with_no_frame_left_for_it_is_survived() {
        let id = "http://gareus.org/oss/lv2/midifilter#channelfilter";
        for (frames, flood) in [(1u32, 1500), (crate::HOST_MAX_BLOCK as u32, 2200)] {
            let Some(mut p) = installed("midifilter.lv2", id) else {
                return;
            };
            let mut events_in = EventList::with_capacity(flood);
            for i in 0..flood {
                let kind = EventKind::Midi([0xb0, 1, (i % 128) as u8]);
                let frame = frames - 1;
                events_in.push(Event { frame, kind }).unwrap();
            }
            let mut events_out = EventList::with_capacity(4096);
            p.process(&mut ProcessContext {
                frames,
                transport: &TransportState::default(),
                audio_in: &[],
                audio_out: &mut [],
                events_in: &events_in,
                events_out: &mut events_out,
            });
            // What one run of the plugin can take in and give back arrived.
            assert!(events_out.len() >= 300, "{} events", events_out.len());
        }
    }

    /// More MIDI than the atom input holds is delivered by running the block
    /// in pieces; the events past the limit (often note-offs) are not lost.
    #[test]
    fn midi_beyond_the_atom_inputs_capacity_still_arrives() {
        let id = "http://gareus.org/oss/lv2/midifilter#channelfilter";
        let Some(mut p) = installed("midifilter.lv2", id) else {
            return;
        };
        // Channel 2 is filtered out, so the flood below costs no output room.
        let channel_2 = p.params.iter().find(|i| i.name == "Channel  2").unwrap().id;
        set(&p, channel_2, 0.0);

        let frames = crate::HOST_MAX_BLOCK as u32;
        let (flood, notes) = (4000u32, 16u8);
        let mut events_in = EventList::with_capacity(flood as usize + usize::from(notes));
        for i in 0..flood {
            let frame = i * (frames - 1) / flood;
            let kind = EventKind::Midi([0xb1, 1, (i % 128) as u8]);
            events_in.push(Event { frame, kind }).unwrap();
        }
        for key in 0..notes {
            let kind = EventKind::Midi([0x80, 60 + key, 0]);
            let frame = frames - 1;
            events_in.push(Event { frame, kind }).unwrap();
        }
        let mut events_out = EventList::with_capacity(64);
        p.process(&mut ProcessContext {
            frames,
            transport: &TransportState::default(),
            audio_in: &[],
            audio_out: &mut [],
            events_in: &events_in,
            events_out: &mut events_out,
        });
        let arrived: Vec<_> = events_out.iter().map(|e| (e.frame, e.kind)).collect();
        let sent: Vec<_> = (0..notes)
            .map(|key| (frames - 1, midi::decode(&[0x80, 60 + key, 0]).unwrap()))
            .collect();
        assert_eq!(arrived, sent);
    }

    /// A UI's control writes reach the host as output parameter events, so
    /// whatever tracks parameters (the sandbox client, automation) sees them.
    #[test]
    fn ui_control_writes_are_reported_to_the_host() {
        let Some(mut p) = mda_delay() else { return };
        assert!(p.ports().event_out);
        let fb = p.params.iter().find(|i| i.name == "Feedback").unwrap().id;
        let to_dsp = &mut p.ui_ends.as_mut().unwrap().to_dsp;
        assert!(ring::write(
            to_dsp,
            fb,
            ring::CONTROL,
            &0.25f32.to_ne_bytes()
        ));
        // Not a parameter: the plugin has no such port.
        assert!(ring::write(
            to_dsp,
            999,
            ring::CONTROL,
            &0.5f32.to_ne_bytes()
        ));

        let frames = 64;
        let mut outs = vec![vec![0.0f32; frames]; 2];
        let mut out_refs: Vec<&mut [f32]> = outs.iter_mut().map(|b| &mut b[..]).collect();
        let events_in = EventList::with_capacity(0);
        let mut events_out = EventList::with_capacity(8);
        p.process(&mut ProcessContext {
            frames: frames as u32,
            transport: &TransportState::default(),
            audio_in: &[],
            audio_out: &mut out_refs,
            events_in: &events_in,
            events_out: &mut events_out,
        });
        assert_eq!(p.param_value(fb), 0.25);
        let params: Vec<_> = events_out
            .iter()
            .filter_map(|e| match e.kind {
                EventKind::ParamValue { param, value } => Some((e.frame, param, value)),
                _ => None,
            })
            .collect();
        assert_eq!(params, [(0, fb, 0.25)]);
    }
}
