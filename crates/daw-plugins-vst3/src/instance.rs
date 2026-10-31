//! A running VST3 plugin: component, edit controller and audio processor behind
//! [`PluginInstance`].

use std::any::Any;
use std::collections::HashMap;
use std::ffi::c_void;
use std::path::Path;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use daw_core::event::{Event as DawEvent, EventKind, ParamId, all_notes_off};
use daw_core::exclusive::Exclusive;
use daw_core::node::{Node, PortLayout, ProcessContext, TransportState};
use daw_core::plugin::{
    ParamInfo, PluginDescriptor, PluginError, PluginFormatKind, PluginInstance, PluginProcessor,
};
use vst3::Steinberg::Vst::{
    AudioBusBuffers, AudioBusBuffers__type0, BusDirections_, BusInfo, ControllerNumbers_,
    Event as VstEvent, Event_::EventTypes_, Event__type0, IAudioProcessor, IAudioProcessorTrait,
    IComponent, IComponentHandler, IComponentTrait, IConnectionPoint, IConnectionPointTrait,
    IEditController, IEditControllerTrait, IMidiMapping, IMidiMappingTrait, MediaTypes_,
    NoteOffEvent, NoteOnEvent, ParamID, ParameterInfo, ParameterInfo_::ParameterFlags_,
    PolyPressureEvent, ProcessContext as VstProcessContext, ProcessContext_::StatesAndFlags_,
    ProcessData, ProcessModes_, ProcessSetup, RestartFlags_, SpeakerArrangement,
    SymbolicSampleSizes_, kNoParamId,
};
use vst3::Steinberg::{
    IBStream, IPluginBaseTrait, IPluginFactoryTrait, TUID, kResultOk, kResultTrue,
};
use vst3::{ComPtr, ComWrapper, Interface};

use crate::editor::Editor;
use crate::host::{
    ComponentHandler, ControllerProxy, Edit, EventQueue, MemoryStream, ParamChanges, host,
    host_context, in_process,
};
use crate::module::{AUDIO_EFFECT_CLASS, ClassInfo, Module};
use crate::util::{tuid_from_hex, tuid_to_hex, wstr_field};

/// Upper bounds for one processing block.
const MAX_EVENTS: usize = 1024;
const MAX_PARAM_POINTS: usize = 64;
const EDIT_QUEUE: usize = 1024;

/// MIDI controller numbers the host maps through `IMidiMapping`: 0..=127 plus aftertouch and
/// pitch bend.
const MIDI_CONTROLLERS: usize = 130;

const STATE_MAGIC: &[u8; 8] = b"KDNVST3\0";
const STATE_VERSION: u32 = 1;

/// Points sampled along a continuous parameter's curve. Enough for linear interpolation
/// between them to follow log and skewed curves to well under a percent.
const CURVE_POINTS: usize = 129;

/// Most zero-sample `process` calls one state save makes to hand editor edits over.
const FLUSH_ROUNDS: usize = 64;

#[derive(Clone, Debug)]
struct Param {
    info: ParamInfo,
    /// Plain values at evenly spaced normalized values, from the plugin's
    /// `normalizedParamToPlain`. Sampled off the real-time thread so `process` converts both
    /// ways without calling into the controller. Always at least two points.
    curve: Box<[f64]>,
}

impl Param {
    /// Samples `plain` (normalized to plain) at one point per step for a discrete parameter
    /// with up to `CURVE_POINTS` steps, else at `CURVE_POINTS` points.
    fn sampled(info: ParamInfo, step_count: i32, plain: impl Fn(f64) -> f64) -> Param {
        let points = match usize::try_from(step_count) {
            Ok(steps) if (1..CURVE_POINTS).contains(&steps) => steps + 1,
            _ => CURVE_POINTS,
        };
        let last = (points - 1) as f64;
        let curve = (0..points).map(|i| plain(i as f64 / last)).collect();
        Param { info, curve }
    }

    fn to_plain(&self, normalized: f64) -> f64 {
        let c = &self.curve;
        let x = normalized.clamp(0.0, 1.0) * (c.len() - 1) as f64;
        let i = (x as usize).min(c.len() - 2);
        c[i] + (x - i as f64) * (c[i + 1] - c[i])
    }

    /// Inverts the sampled curve, assuming the plugin's curve is monotonic.
    fn to_normalized(&self, plain: f64) -> f64 {
        let c = &self.curve;
        let last = c.len() - 1;
        let rising = c[last] >= c[0];
        // The first point at or past `plain`, in the curve's direction.
        let i = c.partition_point(|&v| if rising { v < plain } else { v > plain });
        if i == 0 {
            return 0.0;
        }
        if i > last {
            return 1.0;
        }
        let (a, b) = (c[i - 1], c[i]);
        let t = if b == a {
            0.0
        } else {
            ((plain - a) / (b - a)).clamp(0.0, 1.0)
        };
        ((i - 1) as f64 + t) / last as f64
    }
}

/// The controller's parameter list, shared by the main thread and `process`, with the values
/// the processor saw on their way to the controller off the real-time thread.
///
/// Listed parameters keep only their newest value, so no amount of automation between two
/// main-thread pumps can leave the controller (and `param_value`) on a stale one. Ids the
/// controller does not list go through a small ring that drops what does not fit. A new list
/// is a new `Params`; the old one is drained once `process` has moved over.
struct Params {
    list: Vec<Param>,
    index: HashMap<ParamID, usize>,
    values: Box<[AtomicU64]>,
    dirty: Box<[AtomicBool]>,
}

impl Params {
    fn new(list: Vec<Param>) -> Arc<Params> {
        Arc::new(Params {
            index: list
                .iter()
                .enumerate()
                .map(|(i, p)| (p.info.id, i))
                .collect(),
            values: (0..list.len()).map(|_| AtomicU64::new(0)).collect(),
            dirty: (0..list.len()).map(|_| AtomicBool::new(false)).collect(),
            list,
        })
    }

    fn index(&self, id: ParamID) -> Option<usize> {
        self.index.get(&id).copied()
    }

    fn to_plain(&self, id: ParamID, normalized: f64) -> f64 {
        self.index(id)
            .map_or(normalized, |i| self.list[i].to_plain(normalized))
    }

    /// Real-time safe. `unknown` takes ids the list does not have.
    fn send(&self, unknown: &mut rtrb::Producer<Edit>, edit: Edit) {
        match self.index(edit.id) {
            Some(i) => {
                self.values[i].store(edit.normalized.to_bits(), Ordering::Relaxed);
                self.dirty[i].store(true, Ordering::Release);
            }
            None => {
                let _ = unknown.push(edit);
            }
        }
    }

    /// Takes the newest value of every listed parameter sent since the last call.
    fn drain(&self, mut f: impl FnMut(Edit)) {
        for (i, dirty) in self.dirty.iter().enumerate() {
            if dirty.swap(false, Ordering::Acquire) {
                let normalized = f64::from_bits(self.values[i].load(Ordering::Relaxed));
                f(Edit {
                    id: self.list[i].info.id,
                    normalized,
                });
            }
        }
    }
}

/// The split component's connection to its controller, through a proxy that keeps messages
/// sent from `process` from running controller code on the audio thread.
struct Connection {
    component: ComPtr<IConnectionPoint>,
    controller: ComPtr<IConnectionPoint>,
    proxy: ComWrapper<ControllerProxy>,
}

/// Channel counts of each audio bus, in bus order.
#[derive(Clone, Debug, Default)]
struct Buses {
    inputs: Vec<i32>,
    outputs: Vec<i32>,
    event_in: bool,
    event_out: bool,
}

/// Buffers handed to `IAudioProcessor::process`, built in `prepare`. The raw pointers point
/// into the `Vec`s owned here, which are never resized after construction.
struct Rt {
    in_buses: Vec<AudioBusBuffers>,
    out_buses: Vec<AudioBusBuffers>,
    _in_ptrs: Vec<*mut f32>,
    _out_ptrs: Vec<*mut f32>,
    in_audio: Vec<Vec<f32>>,
    out_audio: Vec<Vec<f32>>,
    in_params: ComWrapper<ParamChanges>,
    out_params: ComWrapper<ParamChanges>,
    in_events: ComWrapper<EventQueue>,
    out_events: ComWrapper<EventQueue>,
    context: VstProcessContext,
    max_frames: u32,
}

impl Rt {
    fn new(buses: &Buses, max_frames: u32, sample_rate: f64) -> Box<Rt> {
        fn bus_buffers(
            channels: &[i32],
            audio: &mut [Vec<f32>],
        ) -> (Vec<AudioBusBuffers>, Vec<*mut f32>) {
            let mut ptrs: Vec<*mut f32> = audio.iter_mut().map(|c| c.as_mut_ptr()).collect();
            let mut at = 0;
            let buses = channels
                .iter()
                .map(|&n| {
                    let bus = AudioBusBuffers {
                        numChannels: n,
                        silenceFlags: 0,
                        __field0: AudioBusBuffers__type0 {
                            channelBuffers32: ptrs[at..].as_mut_ptr(),
                        },
                    };
                    at += n as usize;
                    bus
                })
                .collect();
            (buses, ptrs)
        }

        let frames = max_frames as usize;
        let n_in: i32 = buses.inputs.iter().sum();
        let n_out: i32 = buses.outputs.iter().sum();
        let mut in_audio = vec![vec![0.0f32; frames]; n_in as usize];
        let mut out_audio = vec![vec![0.0f32; frames]; n_out as usize];
        let (in_buses, in_ptrs) = bus_buffers(&buses.inputs, &mut in_audio);
        let (out_buses, out_ptrs) = bus_buffers(&buses.outputs, &mut out_audio);
        let mut context: VstProcessContext = unsafe { std::mem::zeroed() };
        context.sampleRate = sample_rate;
        context.timeSigNumerator = 4;
        context.timeSigDenominator = 4;
        Box::new(Rt {
            in_buses,
            out_buses,
            _in_ptrs: in_ptrs,
            _out_ptrs: out_ptrs,
            in_audio,
            out_audio,
            in_params: ComWrapper::new(ParamChanges::new(MAX_EVENTS, MAX_PARAM_POINTS)),
            out_params: ComWrapper::new(ParamChanges::new(MAX_EVENTS, MAX_PARAM_POINTS)),
            in_events: ComWrapper::new(EventQueue::new(MAX_EVENTS)),
            out_events: ComWrapper::new(EventQueue::new(MAX_EVENTS)),
            context,
            max_frames,
        })
    }

    /// The `process` argument for `frames` samples over these buffers and queues.
    fn process_data(&mut self, frames: i32) -> ProcessData {
        ProcessData {
            processMode: ProcessModes_::kRealtime as i32,
            symbolicSampleSize: SymbolicSampleSizes_::kSample32 as i32,
            numSamples: frames,
            numInputs: self.in_buses.len() as i32,
            numOutputs: self.out_buses.len() as i32,
            inputs: self.in_buses.as_mut_ptr(),
            outputs: self.out_buses.as_mut_ptr(),
            inputParameterChanges: self
                .in_params
                .as_com_ref()
                .map_or(ptr::null_mut(), |r| r.as_ptr()),
            outputParameterChanges: self
                .out_params
                .as_com_ref()
                .map_or(ptr::null_mut(), |r| r.as_ptr()),
            inputEvents: self
                .in_events
                .as_com_ref()
                .map_or(ptr::null_mut(), |r| r.as_ptr()),
            outputEvents: self
                .out_events
                .as_com_ref()
                .map_or(ptr::null_mut(), |r| r.as_ptr()),
            processContext: &mut self.context,
        }
    }
}

impl Audio {
    /// Starts or stops processing, on the main thread.
    fn set_processing(&mut self, on: bool) {
        if self.processing != on {
            // Plugins may refuse to start; they still process, so carry on either way.
            unsafe { self.processor.setProcessing(on as u8) };
            self.processing = on;
        }
    }

    /// Runs on the audio thread, so only cycles `setProcessing`, which the spec allows there
    /// and plugins treat as a state reset. `setActive` and the latency
    /// re-read stay on the main thread, in `prepare` (and `handle_restart` for a changed
    /// layout).
    fn reset(&mut self) {
        if self.processing {
            in_process(|| unsafe {
                self.processor.setProcessing(0);
                self.processor.setProcessing(1);
            });
        }
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        let rt = self.rt.as_deref_mut().filter(|_| !self.io_stale);
        let Some(rt) = rt else {
            silence(ctx);
            return;
        };
        let frames = ctx.frames.min(rt.max_frames) as usize;

        rt.in_params.clear();
        rt.out_params.clear();
        rt.in_events.clear();
        rt.out_events.clear();

        // Edits made in the plugin's editor: to the processor, and out to the host so it can
        // record automation.
        while let Ok(e) = self.edits_rx.pop() {
            rt.in_params.add(e.id, 0, e.normalized);
            let value = self.params.to_plain(e.id, e.normalized);
            let _ = ctx.events_out.push(DawEvent {
                frame: 0,
                kind: EventKind::ParamValue { param: e.id, value },
            });
        }

        for ev in ctx.events_in.iter() {
            let frame = ev.frame.min(frames.saturating_sub(1) as u32);
            let kind = match ev.kind {
                // VST3 has no wildcard note off; send all notes off, which reaches plugins that
                // map CC 123.
                EventKind::NoteOff {
                    channel, key: None, ..
                } => EventKind::Midi(all_notes_off(channel)),
                kind => kind,
            };
            match kind {
                EventKind::NoteOn {
                    channel,
                    key,
                    velocity,
                    note_id,
                } => {
                    rt.in_events.push(vst_event(
                        frame,
                        EventTypes_::kNoteOnEvent,
                        Event__type0 {
                            noteOn: NoteOnEvent {
                                channel: channel as i16,
                                pitch: key as i16,
                                tuning: 0.0,
                                velocity,
                                length: 0,
                                noteId: note_id,
                            },
                        },
                    ));
                }
                EventKind::NoteOff {
                    channel,
                    key: Some(key),
                    velocity,
                    note_id,
                } => {
                    rt.in_events.push(vst_event(
                        frame,
                        EventTypes_::kNoteOffEvent,
                        Event__type0 {
                            noteOff: NoteOffEvent {
                                channel: channel as i16,
                                pitch: key as i16,
                                velocity,
                                noteId: note_id,
                                tuning: 0.0,
                            },
                        },
                    ));
                }
                EventKind::Midi(bytes) => {
                    if let Some(e) = midi_to_vst_event(frame, bytes) {
                        rt.in_events.push(e);
                        continue;
                    }
                    // Controllers, channel pressure and pitch bend become parameter changes
                    // through the plugin's MIDI mapping.
                    let channel = (bytes[0] & 0x0f) as usize;
                    let (cc, normalized) = match bytes[0] & 0xf0 {
                        // Data bytes are 7 bits; a corrupt one must not push the value past 1.
                        0xb0 => (bytes[1] as usize & 0x7f, (bytes[2] & 0x7f) as f64 / 127.0),
                        0xd0 => (
                            ControllerNumbers_::kAfterTouch as usize,
                            (bytes[1] & 0x7f) as f64 / 127.0,
                        ),
                        0xe0 => (
                            ControllerNumbers_::kPitchBend as usize,
                            (((bytes[2] as u32 & 0x7f) << 7) | (bytes[1] as u32 & 0x7f)) as f64
                                / 16383.0,
                        ),
                        _ => continue,
                    };
                    let id = self.midi_map[channel][cc];
                    if id != kNoParamId {
                        rt.in_params.add(id, frame as i32, normalized);
                        self.params
                            .send(&mut self.unknown_tx, Edit { id, normalized });
                    }
                }
                // Turned into all notes off above.
                EventKind::NoteOff { key: None, .. } => {}
                EventKind::ParamValue { param, value } => {
                    let normalized = self.params.index(param).map_or(value.clamp(0.0, 1.0), |i| {
                        self.params.list[i].to_normalized(value)
                    });
                    rt.in_params.add(param, frame as i32, normalized);
                    self.params.send(
                        &mut self.unknown_tx,
                        Edit {
                            id: param,
                            normalized,
                        },
                    );
                }
            }
        }

        if !self.event_in {
            // Notes have nowhere to go without an event bus.
            rt.in_events.clear();
        }

        for (i, buf) in rt.in_audio.iter_mut().enumerate() {
            let buf = &mut buf[..frames];
            match ctx.audio_in.get(i) {
                Some(src) => {
                    let n = src.len().min(frames);
                    buf[..n].copy_from_slice(&src[..n]);
                    buf[n..].fill(0.0);
                }
                None => buf.fill(0.0),
            }
        }
        for bus in rt.in_buses.iter_mut() {
            bus.silenceFlags = 0;
        }
        for bus in rt.out_buses.iter_mut() {
            bus.silenceFlags = 0;
        }

        fill_context(&mut rt.context, ctx.transport, self.sample_rate);

        let mut data = rt.process_data(frames as i32);
        in_process(|| unsafe { self.processor.process(&mut data) });
        rt.context.continousTimeSamples += frames as i64;

        for (i, out) in ctx.audio_out.iter_mut().enumerate() {
            let n = out.len().min(frames);
            match rt.out_audio.get(i) {
                Some(src) => out[..n].copy_from_slice(&src[..n]),
                None => out[..n].fill(0.0),
            }
            out[n..].fill(0.0);
        }

        for q in rt.out_params.queues() {
            let id = q.id();
            let Some((offset, normalized)) = q.last() else {
                continue;
            };
            self.params
                .send(&mut self.unknown_tx, Edit { id, normalized });
            let value = self.params.to_plain(id, normalized);
            let _ = ctx.events_out.push(DawEvent {
                frame: offset.max(0) as u32,
                kind: EventKind::ParamValue { param: id, value },
            });
        }

        for e in rt.out_events.iter() {
            let frame = e.sampleOffset.max(0) as u32;
            let kind = unsafe {
                match e.r#type as vst3::Steinberg::Vst::Event_::EventTypes {
                    EventTypes_::kNoteOnEvent => {
                        let n = e.__field0.noteOn;
                        EventKind::NoteOn {
                            channel: n.channel as u8,
                            key: n.pitch as u8,
                            velocity: n.velocity,
                            note_id: n.noteId,
                        }
                    }
                    EventTypes_::kNoteOffEvent => {
                        let n = e.__field0.noteOff;
                        EventKind::NoteOff {
                            channel: n.channel as u8,
                            // A negative pitch names no key; read it as every key, as CLAP does.
                            key: (n.pitch >= 0).then_some(n.pitch as u8),
                            velocity: n.velocity,
                            note_id: n.noteId,
                        }
                    }
                    EventTypes_::kPolyPressureEvent => {
                        let p = e.__field0.polyPressure;
                        EventKind::Midi([
                            0xa0 | (p.channel as u8 & 0x0f),
                            p.pitch as u8 & 0x7f,
                            (p.pressure.clamp(0.0, 1.0) * 127.0).round() as u8,
                        ])
                    }
                    EventTypes_::kLegacyMIDICCOutEvent => {
                        let m = e.__field0.midiCCOut;
                        let ch = m.channel as u8 & 0x0f;
                        let (v1, v2) = (m.value as u8 & 0x7f, m.value2 as u8 & 0x7f);
                        match m.controlNumber as i32 {
                            c if c < 128 => EventKind::Midi([0xb0 | ch, c as u8, v1]),
                            c if c == ControllerNumbers_::kAfterTouch as i32 => {
                                EventKind::Midi([0xd0 | ch, v1, 0])
                            }
                            c if c == ControllerNumbers_::kPitchBend as i32 => {
                                EventKind::Midi([0xe0 | ch, v1, v2])
                            }
                            c if c == ControllerNumbers_::kCtrlProgramChange as i32 => {
                                EventKind::Midi([0xc0 | ch, v1, 0])
                            }
                            _ => continue,
                        }
                    }
                    _ => continue,
                }
            };
            let _ = ctx.events_out.push(DawEvent { frame, kind });
        }
    }
}

pub struct Vst3Instance {
    descriptor: PluginDescriptor,
    editor: Option<Editor>,
    /// From `set_editor_title`; the plugin's name until then.
    editor_title: Option<String>,
    /// The same list `process` uses; replaced together with `audio`'s.
    params: Arc<Params>,
    /// Values for ids `params` does not list, from `process` to the controller.
    unknown_rx: Mutex<rtrb::Consumer<Edit>>,
    buses: Buses,
    sample_rate: f64,
    max_frames: u32,
    latency: u32,
    active: bool,
    /// The plugin asked for a new port layout or latency since the last `prepare`. Reported
    /// once through `take_layout_changed`.
    layout_changed: bool,
    /// Whether the plugin had parameters at the last `prepare`, which makes its ports take
    /// and send events. Fixed between prepares like the rest of the layout.
    has_params: bool,
    /// The speaker arrangements `prepare` settled on, inputs then outputs.
    arrangements: (Vec<SpeakerArrangement>, Vec<SpeakerArrangement>),

    /// What `process` runs on, shared with the [`Vst3Processor`] that `split` hands out.
    audio: Arc<Exclusive<Audio>>,
    /// The processor half is out, so this object's own `process` and `reset` do nothing.
    lent: bool,

    connection: Option<Connection>,
    controller: ComPtr<IEditController>,
    /// False when the component implements the controller itself.
    split: bool,
    component: ComPtr<IComponent>,
    handler: ComWrapper<ComponentHandler>,
    // Modules stay loaded for the life of the process; this only keeps the cache honest.
    _module: Arc<Module>,
}

// SAFETY: the engine never uses an instance from two threads at once (every method takes
// `&mut self` except the read-only `params` and `param_value`, and `param_value` reaches the
// processor's values through atomics and a mutex). The controller and component are only
// called from the main thread; `process` goes through `audio`.
unsafe impl Send for Vst3Instance {}

/// What `process` works on. The instance and its [`Vst3Processor`] share it through an
/// [`Exclusive`]: the audio thread takes it for one block, and the main thread only for the
/// calls the spec keeps apart from `process` (activation, bus changes, the parameter flush)
/// and to swap in a new parameter list or MIDI mapping, each brief. Everything else the
/// main thread does (state, editors, the controller) runs while the plugin processes, as
/// VST3 allows.
struct Audio {
    processor: ComPtr<IAudioProcessor>,
    rt: Option<Box<Rt>>,
    /// The plugin's buses no longer match `rt`, so `process` outputs silence until `prepare`.
    io_stale: bool,
    processing: bool,
    event_in: bool,
    sample_rate: f64,
    params: Arc<Params>,
    /// `[channel][controller]` to parameter, or `kNoParamId`.
    midi_map: Box<[[ParamID; MIDI_CONTROLLERS]; 16]>,
    /// Edits from the plugin's editor.
    edits_rx: rtrb::Consumer<Edit>,
    unknown_tx: rtrb::Producer<Edit>,
}

// SAFETY: the raw pointers in `Rt` only point at buffers owned by the same `Audio`, and the
// `Exclusive` it lives in lets one thread at a time call the processor.
unsafe impl Send for Audio {}

/// The audio-thread half of a [`Vst3Instance`], from [`PluginInstance::split`].
///
/// A block that comes while the main thread is in one of the few calls that must not
/// overlap `process` waits a few microseconds for it, then is silent and loses its events;
/// a `reset` then is skipped. Join it before dropping the instance, or the plugin's last
/// reference goes wherever this is dropped.
pub struct Vst3Processor(Arc<Exclusive<Audio>>);

/// How many times a [`Vst3Processor`] block retries the audio state the main thread holds.
const AUDIO_RETRIES: u32 = 100;

impl PluginProcessor for Vst3Processor {
    fn process(&mut self, ctx: &mut ProcessContext) {
        for _ in 0..AUDIO_RETRIES {
            if self.0.try_audio(|a| a.process(ctx)).is_some() {
                return;
            }
            std::hint::spin_loop();
        }
        silence(ctx);
    }

    fn reset(&mut self) {
        self.0.try_audio(Audio::reset);
    }

    fn into_any(self: Box<Self>) -> Box<dyn Any + Send> {
        self
    }
}

fn silence(ctx: &mut ProcessContext) {
    for out in ctx.audio_out.iter_mut() {
        out.fill(0.0);
    }
}

fn load_err(what: &str) -> PluginError {
    PluginError::Load(what.to_string())
}

unsafe fn create<I: Interface>(module: &Module, cid: &TUID) -> Option<ComPtr<I>> {
    let mut obj: *mut c_void = ptr::null_mut();
    let iid = I::IID;
    let result = unsafe {
        module
            .factory()
            .createInstance(cid.as_ptr(), iid.as_ptr() as *const _, &mut obj)
    };
    if result != kResultOk {
        return None;
    }
    unsafe { ComPtr::from_raw(obj as *mut I) }
}

fn audio_channels(component: &ComPtr<IComponent>, dir: i32) -> Vec<i32> {
    unsafe {
        let count = component.getBusCount(MediaTypes_::kAudio as i32, dir);
        (0..count)
            .map(|i| {
                let mut info: BusInfo = std::mem::zeroed();
                if component.getBusInfo(MediaTypes_::kAudio as i32, dir, i, &mut info) == kResultOk
                {
                    info.channelCount.max(0)
                } else {
                    0
                }
            })
            .collect()
    }
}

fn read_buses(component: &ComPtr<IComponent>) -> Buses {
    let input = BusDirections_::kInput as i32;
    let output = BusDirections_::kOutput as i32;
    unsafe {
        Buses {
            inputs: audio_channels(component, input),
            outputs: audio_channels(component, output),
            event_in: component.getBusCount(MediaTypes_::kEvent as i32, input) > 0,
            event_out: component.getBusCount(MediaTypes_::kEvent as i32, output) > 0,
        }
    }
}

fn channel_total(channels: &[i32]) -> u16 {
    channels
        .iter()
        .map(|&c| c as u32)
        .sum::<u32>()
        .min(u16::MAX as u32) as u16
}

/// Lists the processor classes of one module as descriptors. Classes whose component cannot be
/// created are left out; when that is every class, the scan fails, naming the first.
pub fn scan_module(path: &Path) -> Result<Vec<PluginDescriptor>, PluginError> {
    let module = Module::load(path).map_err(PluginError::Load)?;
    let factory_vendor = module.factory_vendor();
    let mut found = Vec::new();
    let mut first_error = None;
    for class in module
        .classes()
        .into_iter()
        .filter(|c| c.category == AUDIO_EFFECT_CLASS)
    {
        match probe_ports(&module, &class) {
            Some(ports) => found.push(descriptor_for(path, &class, &factory_vendor, ports)),
            None => {
                let e = format!(
                    "{}: {} could not be created and initialized",
                    path.display(),
                    class.name
                );
                log::warn!("vst3 scan: {e}");
                first_error.get_or_insert(e);
            }
        }
    }
    match first_error {
        Some(e) if found.is_empty() => Err(PluginError::Load(e)),
        _ => Ok(found),
    }
}

fn descriptor_for(
    path: &Path,
    class: &ClassInfo,
    vendor: &str,
    ports: (u16, u16),
) -> PluginDescriptor {
    PluginDescriptor {
        format: PluginFormatKind::Vst3,
        id: tuid_to_hex(&class.cid),
        name: class.name.clone(),
        vendor: if class.vendor.is_empty() {
            vendor.to_string()
        } else {
            class.vendor.clone()
        },
        path: path.to_path_buf(),
        is_instrument: class
            .sub_categories
            .split('|')
            .any(|s| s == "Instrument" || s.starts_with("Instrument")),
        ports,
    }
}

fn probe_ports(module: &Module, class: &ClassInfo) -> Option<(u16, u16)> {
    let component = unsafe { create::<IComponent>(module, &class.cid)? };
    unsafe {
        if component.initialize(host_context()) != kResultOk {
            return None;
        }
        let buses = read_buses(&component);
        component.terminate();
        Some((channel_total(&buses.inputs), channel_total(&buses.outputs)))
    }
}

impl Vst3Instance {
    /// Loads the module named by `d.path` and creates the class `d.id`. The instance is not
    /// processing until [`Node::prepare`] is called.
    pub fn new(d: &PluginDescriptor) -> Result<Self, PluginError> {
        let module = Module::load(&d.path).map_err(PluginError::Load)?;
        let cid = tuid_from_hex(&d.id).ok_or_else(|| load_err("bad class id"))?;

        let context = host_context();
        let (edits_tx, edits_rx) = rtrb::RingBuffer::new(EDIT_QUEUE);
        let handler = ComWrapper::new(ComponentHandler::new(edits_tx));

        unsafe {
            let component = create::<IComponent>(&module, &cid)
                .ok_or_else(|| load_err("factory could not create the component"))?;
            if component.initialize(context) != kResultOk {
                return Err(load_err("component failed to initialize"));
            }
            let Some(processor) = component.cast::<IAudioProcessor>() else {
                component.terminate();
                return Err(load_err("component is not an audio processor"));
            };

            // Single-component plugins implement the controller on the same object.
            let (controller, split) = match component.cast::<IEditController>() {
                Some(c) => (c, false),
                None => {
                    let mut controller_cid: TUID = [0; 16];
                    let controller = (component.getControllerClassId(&mut controller_cid)
                        == kResultOk)
                        .then(|| create::<IEditController>(&module, &controller_cid))
                        .flatten();
                    let Some(controller) = controller else {
                        component.terminate();
                        return Err(load_err("plugin has no edit controller"));
                    };
                    if controller.initialize(context) != kResultOk {
                        component.terminate();
                        return Err(load_err("controller failed to initialize"));
                    }
                    (controller, true)
                }
            };

            let connection = if split {
                match (
                    component.cast::<IConnectionPoint>(),
                    controller.cast::<IConnectionPoint>(),
                ) {
                    (Some(component), Some(controller)) => {
                        let proxy = ComWrapper::new(ControllerProxy::new(controller.clone()));
                        if let Some(p) = proxy.as_com_ref::<IConnectionPoint>() {
                            component.connect(p.as_ptr());
                        }
                        controller.connect(component.as_ptr());
                        Some(Connection {
                            component,
                            controller,
                            proxy,
                        })
                    }
                    _ => None,
                }
            } else {
                None
            };

            if let Some(h) = handler.to_com_ptr::<IComponentHandler>() {
                controller.setComponentHandler(h.as_ptr());
            }

            let (unknown_tx, unknown_rx) = rtrb::RingBuffer::new(EDIT_QUEUE);
            let params = Params::new(Vec::new());
            let buses = read_buses(&component);
            let audio = Audio {
                processor,
                rt: None,
                io_stale: false,
                processing: false,
                event_in: buses.event_in,
                sample_rate: 0.0,
                params: Arc::clone(&params),
                midi_map: Box::new([[kNoParamId; MIDI_CONTROLLERS]; 16]),
                edits_rx,
                unknown_tx,
            };
            let mut instance = Vst3Instance {
                descriptor: d.clone(),
                editor: None,
                editor_title: None,
                params,
                unknown_rx: Mutex::new(unknown_rx),
                buses,
                sample_rate: 0.0,
                max_frames: 0,
                latency: 0,
                active: false,
                layout_changed: false,
                arrangements: Default::default(),
                has_params: false,
                audio: Arc::new(Exclusive::new(audio)),
                lent: false,
                connection,
                controller,
                split,
                component,
                handler,
                _module: module,
            };
            instance.sync_controller_to_component();
            instance.read_params();
            instance.has_params = !instance.params.list.is_empty();
            instance.read_midi_map();
            instance.descriptor.ports = (
                channel_total(&instance.buses.inputs),
                channel_total(&instance.buses.outputs),
            );
            Ok(instance)
        }
    }

    /// Gives a split controller the component's current state, as the spec requires after
    /// creation and after every component `setState`.
    fn sync_controller_to_component(&self) {
        if !self.split {
            return;
        }
        let stream = ComWrapper::new(MemoryStream::default());
        let Some(s) = stream.to_com_ptr::<IBStream>() else {
            return;
        };
        unsafe {
            if self.component.getState(s.as_ptr()) == kResultOk {
                stream.rewind();
                self.controller.setComponentState(s.as_ptr());
            }
        }
    }

    /// Runs `f` with the audio side, after the audio thread finishes its block, keeping it
    /// silent meanwhile. Keep `f` short, and never call this from inside `f`.
    fn with_audio<R>(&mut self, f: impl FnOnce(&mut Self, &mut Audio) -> R) -> R {
        let audio = Arc::clone(&self.audio);
        audio.main(|a| f(self, a))
    }

    fn read_params(&mut self) {
        let mut list = Vec::new();
        unsafe {
            let count = self.controller.getParameterCount();
            for i in 0..count {
                let mut info: ParameterInfo = std::mem::zeroed();
                if self.controller.getParameterInfo(i, &mut info) != kResultOk {
                    continue;
                }
                let default = self
                    .controller
                    .normalizedParamToPlain(info.id, info.defaultNormalizedValue);
                let flags = info.flags;
                let automatable = flags & ParameterFlags_::kCanAutomate as i32 != 0
                    && flags & ParameterFlags_::kIsReadOnly as i32 == 0;
                let mut param = Param::sampled(
                    ParamInfo {
                        id: info.id,
                        name: wstr_field(&info.title),
                        min: 0.0,
                        max: 0.0,
                        default,
                        automatable,
                    },
                    info.stepCount,
                    |n| self.controller.normalizedParamToPlain(info.id, n),
                );
                param.info.min = param.to_plain(0.0);
                param.info.max = param.to_plain(1.0);
                list.push(param);
            }
        }
        let new = Params::new(list);
        let old = std::mem::replace(&mut self.params, Arc::clone(&new));
        self.audio.main(|a| a.params = new);
        // `process` sends to the new list from here on; values still in the old one are
        // its last.
        self.flush_to_controller_from(&old);
    }

    fn read_midi_map(&mut self) {
        let Some(mapping) = self.controller.cast::<IMidiMapping>() else {
            return;
        };
        let mut map = Box::new([[kNoParamId; MIDI_CONTROLLERS]; 16]);
        for (channel, row) in map.iter_mut().enumerate() {
            for (cc, slot) in row.iter_mut().enumerate() {
                let mut id: ParamID = kNoParamId;
                let found = unsafe {
                    mapping.getMidiControllerAssignment(0, channel as i16, cc as i16, &mut id)
                };
                if found == kResultOk || found == kResultTrue {
                    *slot = id;
                }
            }
        }
        let old = self.audio.main(|a| std::mem::replace(&mut a.midi_map, map));
        drop(old);
    }

    fn deactivate(&mut self, a: &mut Audio) {
        a.set_processing(false);
        if self.active {
            unsafe { self.component.setActive(0) };
            self.active = false;
        }
    }

    fn activate(&mut self, a: &mut Audio) {
        unsafe {
            if !self.active {
                self.component.setActive(1);
                self.active = true;
            }
            self.latency = a.processor.getLatencySamples();
        }
        a.set_processing(true);
    }

    /// Applies processor-side values to the controller so editors and `param_value` follow.
    fn flush_to_controller(&self) {
        self.flush_to_controller_from(&self.params);
    }

    fn flush_to_controller_from(&self, params: &Params) {
        let set = |e: Edit| unsafe {
            self.controller.setParamNormalized(e.id, e.normalized);
        };
        params.drain(set);
        let mut rx = self.unknown_rx.lock().unwrap_or_else(|e| e.into_inner());
        while let Ok(e) = rx.pop() {
            set(e);
        }
    }

    /// Hands editor edits still queued for the processor over with a zero-sample `process`,
    /// the spec's parameter flush, so a state saved now includes them. Edits flushed this
    /// way are not reported to the host as `ParamValue` events.
    fn flush_edits(&mut self, a: &mut Audio) {
        // While stale, `rt`'s buffers no longer match the plugin's buses.
        if !a.processing || a.io_stale {
            return;
        }
        let Some(rt) = a.rt.as_deref_mut() else {
            return;
        };
        // Bounded, so a plugin that calls `performEdit` from its own `process` cannot keep
        // the loop going forever. Each round empties the ring and refills it from overflow,
        // which holds at most one edit per parameter.
        for _ in 0..FLUSH_ROUNDS {
            self.handler.retry_overflow();
            if a.edits_rx.is_empty() {
                return;
            }
            rt.in_params.clear();
            rt.out_params.clear();
            rt.in_events.clear();
            rt.out_events.clear();
            while let Ok(e) = a.edits_rx.pop() {
                rt.in_params.add(e.id, 0, e.normalized);
            }
            let mut data = rt.process_data(0);
            unsafe { a.processor.process(&mut data) };
            for q in rt.out_params.queues() {
                if let Some((_, normalized)) = q.last() {
                    a.params.send(
                        &mut a.unknown_tx,
                        Edit {
                            id: q.id(),
                            normalized,
                        },
                    );
                }
            }
        }
    }

    /// Asks the plugin to go back to the arrangements from `prepare` after it changed its
    /// buses; `process` stays silent until the next `prepare` if it will not. Returns false
    /// before the first `prepare`, when there is no layout to keep.
    fn keep_layout(&mut self, a: &mut Audio) -> bool {
        if a.rt.is_none() {
            return false;
        }
        let old = (self.buses.inputs.clone(), self.buses.outputs.clone());
        let (mut ins, mut outs) = self.arrangements.clone();
        self.deactivate(a);
        let now = read_buses(&self.component);
        let kept = now.inputs.len() == ins.len()
            && now.outputs.len() == outs.len()
            && unsafe {
                a.processor.setBusArrangements(
                    ins.as_mut_ptr(),
                    ins.len() as i32,
                    outs.as_mut_ptr(),
                    outs.len() as i32,
                )
            } == kResultTrue
            && {
                let now = read_buses(&self.component);
                (now.inputs, now.outputs) == old
            };
        // Back to processing either way.
        self.activate(a);
        a.io_stale = !kept;
        true
    }

    fn prepare_with(&mut self, a: &mut Audio, sample_rate: f64, max_frames: u32) {
        self.deactivate(a);
        self.layout_changed = false;
        a.io_stale = false;

        // The plugin may have added or removed buses since they were last read.
        self.buses = read_buses(&self.component);
        self.sample_rate = sample_rate;
        self.max_frames = max_frames.max(1);
        unsafe {
            let mut setup = ProcessSetup {
                processMode: ProcessModes_::kRealtime as i32,
                symbolicSampleSize: SymbolicSampleSizes_::kSample32 as i32,
                maxSamplesPerBlock: self.max_frames as i32,
                sampleRate: sample_rate,
            };
            a.processor.setupProcessing(&mut setup);

            // Offer the plugin its own default arrangements, then use whatever it ends up with.
            let arrangements = |dir: i32, n: usize| -> Vec<SpeakerArrangement> {
                (0..n)
                    .map(|i| {
                        let mut arr: SpeakerArrangement = 0;
                        a.processor.getBusArrangement(dir, i as i32, &mut arr);
                        arr
                    })
                    .collect()
            };
            let mut ins = arrangements(BusDirections_::kInput as i32, self.buses.inputs.len());
            let mut outs = arrangements(BusDirections_::kOutput as i32, self.buses.outputs.len());
            a.processor.setBusArrangements(
                ins.as_mut_ptr(),
                ins.len() as i32,
                outs.as_mut_ptr(),
                outs.len() as i32,
            );
            self.buses = read_buses(&self.component);
            self.arrangements = (
                arrangements(BusDirections_::kInput as i32, self.buses.inputs.len()),
                arrangements(BusDirections_::kOutput as i32, self.buses.outputs.len()),
            );

            for (media, dir, n) in [
                (
                    MediaTypes_::kAudio,
                    BusDirections_::kInput,
                    self.buses.inputs.len(),
                ),
                (
                    MediaTypes_::kAudio,
                    BusDirections_::kOutput,
                    self.buses.outputs.len(),
                ),
            ] {
                for i in 0..n {
                    self.component
                        .activateBus(media as i32, dir as i32, i as i32, 1);
                }
            }
            for dir in [BusDirections_::kInput, BusDirections_::kOutput] {
                let n = self
                    .component
                    .getBusCount(MediaTypes_::kEvent as i32, dir as i32);
                for i in 0..n {
                    self.component
                        .activateBus(MediaTypes_::kEvent as i32, dir as i32, i, 1);
                }
            }
        }
        self.has_params = !self.params.list.is_empty();
        a.event_in = self.buses.event_in;
        a.sample_rate = sample_rate;
        a.rt = Some(Rt::new(&self.buses, self.max_frames, sample_rate));
        self.activate(a);
    }

    fn handle_restart(&mut self) {
        let flags = self.handler.take_restart_flags();
        if flags == 0 {
            return;
        }
        // The engine reads ports and latency once per prepare, so neither changes here: the
        // host learns through `take_layout_changed` and may prepare again, which renegotiates
        // the buses and does the deactivate/activate cycle the spec asks for before reading
        // the new latency. Until then the plugin is asked to keep the layout the engine has.
        if flags & (RestartFlags_::kIoChanged as i32) != 0
            && self.with_audio(|p, a| p.keep_layout(a))
        {
            self.layout_changed = true;
        }
        if flags & (RestartFlags_::kLatencyChanged as i32) != 0 && self.active {
            self.layout_changed = true;
        }
        if flags
            & (RestartFlags_::kParamValuesChanged as i32
                | RestartFlags_::kParamTitlesChanged as i32)
            != 0
        {
            if flags & (RestartFlags_::kParamValuesChanged as i32) != 0 {
                // A preset the plugin loaded itself, say.
                self.handler.mark_state_changed();
            }
            self.read_params();
            // Gaining the first parameter or losing the last changes the event ports.
            if self.params.list.is_empty() == self.has_params {
                self.layout_changed = true;
            }
        }
        if flags & (RestartFlags_::kMidiCCAssignmentChanged as i32) != 0 {
            self.read_midi_map();
        }
    }
}

fn midi_to_vst_event(frame: u32, bytes: [u8; 3]) -> Option<VstEvent> {
    let channel = (bytes[0] & 0x0f) as i16;
    let pitch = (bytes[1] & 0x7f) as i16;
    let value = (bytes[2] & 0x7f) as f32 / 127.0;
    let body = match bytes[0] & 0xf0 {
        0x90 if bytes[2] > 0 => (
            EventTypes_::kNoteOnEvent,
            Event__type0 {
                noteOn: NoteOnEvent {
                    channel,
                    pitch,
                    tuning: 0.0,
                    velocity: value,
                    length: 0,
                    noteId: -1,
                },
            },
        ),
        0x80 | 0x90 => (
            EventTypes_::kNoteOffEvent,
            Event__type0 {
                noteOff: NoteOffEvent {
                    channel,
                    pitch,
                    velocity: value,
                    noteId: -1,
                    tuning: 0.0,
                },
            },
        ),
        0xa0 => (
            EventTypes_::kPolyPressureEvent,
            Event__type0 {
                polyPressure: PolyPressureEvent {
                    channel,
                    pitch,
                    pressure: value,
                    noteId: -1,
                },
            },
        ),
        _ => return None,
    };
    Some(vst_event(frame, body.0, body.1))
}

fn vst_event(
    frame: u32,
    kind: vst3::Steinberg::Vst::Event_::EventTypes,
    body: Event__type0,
) -> VstEvent {
    VstEvent {
        busIndex: 0,
        sampleOffset: frame as i32,
        ppqPosition: 0.0,
        flags: 0,
        r#type: kind as u16,
        __field0: body,
    }
}

impl Node for Vst3Instance {
    fn ports(&self) -> PortLayout {
        PortLayout {
            audio_in: channel_total(&self.buses.inputs),
            audio_out: channel_total(&self.buses.outputs),
            // Parameter changes travel as events too, so a plugin with parameters takes and
            // sends events even without event buses (most effects); notes need a bus.
            event_in: self.buses.event_in || self.has_params,
            event_out: self.buses.event_out || self.has_params,
        }
    }

    /// Safe with the processor half out too, though the host joins it first: the audio
    /// thread stays out until this is done.
    fn prepare(&mut self, sample_rate: f64, max_frames: u32) {
        self.with_audio(|p, a| p.prepare_with(a, sample_rate, max_frames));
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        if self.lent || self.audio.try_audio(|a| a.process(ctx)).is_none() {
            silence(ctx);
        }
    }

    fn latency(&self) -> u32 {
        self.latency
    }

    fn reset(&mut self) {
        if !self.lent {
            self.audio.try_audio(Audio::reset);
        }
    }
}

impl PluginInstance for Vst3Instance {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    fn params(&self) -> Vec<ParamInfo> {
        self.params.list.iter().map(|p| p.info.clone()).collect()
    }

    fn param_value(&self, id: ParamId) -> f64 {
        self.flush_to_controller();
        let normalized = unsafe { self.controller.getParamNormalized(id) };
        self.params.to_plain(id, normalized)
    }

    /// Format: magic, version, then the component and controller states as length-prefixed
    /// blobs (little endian).
    fn save_state(&mut self) -> Result<Vec<u8>, PluginError> {
        // Only edits need the flush, and it keeps the audio thread out while it runs.
        if self.handler.has_edits() {
            self.with_audio(|p, a| p.flush_edits(a));
        }
        let read = |get: &dyn Fn(*mut IBStream) -> i32| -> Result<Vec<u8>, PluginError> {
            let stream = ComWrapper::new(MemoryStream::default());
            let s = stream
                .to_com_ptr::<IBStream>()
                .ok_or_else(|| PluginError::State("stream".into()))?;
            if get(s.as_ptr()) != kResultOk {
                return Err(PluginError::State(
                    "plugin refused to save its state".into(),
                ));
            }
            Ok(stream.take())
        };
        let component = read(&|s| unsafe { self.component.getState(s) })?;
        // A single-component plugin has one state; asking again would just duplicate it.
        let controller = if self.split {
            read(&|s| unsafe { self.controller.getState(s) }).unwrap_or_default()
        } else {
            Vec::new()
        };
        let mut out = Vec::with_capacity(28 + component.len() + controller.len());
        out.extend_from_slice(STATE_MAGIC);
        out.extend_from_slice(&STATE_VERSION.to_le_bytes());
        for blob in [&component, &controller] {
            out.extend_from_slice(&(blob.len() as u64).to_le_bytes());
            out.extend_from_slice(blob);
        }
        Ok(out)
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), PluginError> {
        let bad = || PluginError::State("not a VST3 state blob".into());
        let rest = data.strip_prefix(STATE_MAGIC.as_slice()).ok_or_else(bad)?;
        let (version, mut rest) = rest.split_at_checked(4).ok_or_else(bad)?;
        if u32::from_le_bytes(version.try_into().unwrap()) != STATE_VERSION {
            return Err(PluginError::State("unsupported VST3 state version".into()));
        }
        let mut blob = || -> Result<&[u8], PluginError> {
            let (len, tail) = rest.split_at_checked(8).ok_or_else(bad)?;
            let len = u64::from_le_bytes(len.try_into().unwrap()) as usize;
            let (b, tail) = tail.split_at_checked(len).ok_or_else(bad)?;
            rest = tail;
            Ok(b)
        };
        let component = blob()?.to_vec();
        let controller = blob()?.to_vec();

        let write =
            |bytes: Vec<u8>, set: &dyn Fn(*mut IBStream) -> i32| -> Result<(), PluginError> {
                let stream = ComWrapper::new(MemoryStream::with_data(bytes));
                let s = stream
                    .to_com_ptr::<IBStream>()
                    .ok_or_else(|| PluginError::State("stream".into()))?;
                if set(s.as_ptr()) != kResultOk {
                    return Err(PluginError::State("plugin rejected the state".into()));
                }
                Ok(())
            };
        // Editor edits made before the load are stale and must not reach `process` on top of
        // the restored state; with the half out the audio thread could apply them before the
        // clear below, so drop them first.
        self.handler.discard_overflow();
        self.with_audio(|_, a| while a.edits_rx.pop().is_ok() {});
        write(component.clone(), &|s| unsafe {
            self.component.setState(s)
        })?;
        // The processor has its state now. Controllers may answer kNotImplemented (JUCE's
        // wrapper does for setComponentState), so a refusal here only costs the controller's
        // view of the values, not the restore.
        if self.split {
            if let Err(e) = write(component, &|s| unsafe {
                self.controller.setComponentState(s)
            }) {
                log::warn!(
                    "vst3 {}: controller setComponentState: {e}",
                    self.descriptor.name
                );
            }
            if !controller.is_empty()
                && let Err(e) = write(controller, &|s| unsafe { self.controller.setState(s) })
            {
                log::warn!("vst3 {}: controller setState: {e}", self.descriptor.name);
            }
        }
        // Values pushed before the load are stale now, in both directions.
        self.handler.discard_overflow();
        self.with_audio(|p, a| {
            while a.edits_rx.pop().is_ok() {}
            a.params.drain(|_| {});
            let mut rx = p.unknown_rx.lock().unwrap_or_else(|e| e.into_inner());
            while rx.pop().is_ok() {}
        });
        // The restarts and edits the load itself caused are not changes of the plugin's own.
        self.handle_restart();
        self.handler.take_state_changed();
        Ok(())
    }

    fn has_editor(&self) -> bool {
        if self.editor.is_some() {
            return true;
        }
        Editor::probe(&self.controller)
    }

    fn open_editor(&mut self) -> Result<(), PluginError> {
        if self.editor.is_some() {
            return Ok(());
        }
        let title = self
            .editor_title
            .as_deref()
            .unwrap_or(&self.descriptor.name);
        let editor = Editor::open(&self.controller, title).map_err(PluginError::Editor)?;
        self.editor = Some(editor);
        Ok(())
    }

    fn close_editor(&mut self) {
        self.editor = None;
    }

    fn is_editor_open(&self) -> bool {
        self.editor.is_some()
    }

    fn set_editor_title(&mut self, title: &str) {
        if self.editor_title.as_deref() == Some(title) {
            return;
        }
        if let Some(editor) = &self.editor {
            editor.set_title(title);
        }
        self.editor_title = Some(title.to_owned());
    }

    fn take_state_changed(&mut self) -> bool {
        self.handler.take_state_changed()
    }

    fn on_main_thread(&mut self) {
        host().run_loop.pump();
        self.handler.retry_overflow();
        self.flush_to_controller();
        self.handle_restart();
        if let Some(editor) = &mut self.editor
            && !editor.pump()
        {
            self.editor = None;
        }
    }

    fn take_layout_changed(&mut self) -> bool {
        std::mem::take(&mut self.layout_changed)
    }

    fn split(&mut self) -> Option<Box<dyn PluginProcessor>> {
        // Before the first `prepare` there is nothing to process.
        if self.lent || self.sample_rate == 0.0 {
            return None;
        }
        self.lent = true;
        Some(Box::new(Vst3Processor(Arc::clone(&self.audio))))
    }

    fn join(&mut self, processor: Box<dyn PluginProcessor>) {
        match processor.into_any().downcast::<Vst3Processor>() {
            Ok(half) if Arc::ptr_eq(&half.0, &self.audio) => self.lent = false,
            _ => log::warn!(
                "vst3 {}: asked to join a processor that is not its own",
                self.descriptor.name
            ),
        }
    }
}

impl Drop for Vst3Instance {
    fn drop(&mut self) {
        self.editor = None;
        if self.lent {
            log::warn!(
                "vst3 {}: dropped with its processor half still out",
                self.descriptor.name
            );
        }
        self.with_audio(|p, a| {
            p.deactivate(a);
            a.rt = None;
        });
        unsafe {
            if let Some(c) = self.connection.take() {
                if let Some(p) = c.proxy.as_com_ref::<IConnectionPoint>() {
                    c.component.disconnect(p.as_ptr());
                }
                c.controller.disconnect(c.component.as_ptr());
            }
            self.controller.setComponentHandler(ptr::null_mut());
            if self.split {
                self.controller.terminate();
            }
            self.component.terminate();
        }
    }
}

/// Fills the transport part of a VST3 process context. Continuous time is
/// the caller's.
fn fill_context(c: &mut VstProcessContext, transport: &TransportState, sample_rate: f64) {
    let t = transport.or_default_tempo(sample_rate);
    let m = &t.music;
    c.state = StatesAndFlags_::kContTimeValid as u32
        | StatesAndFlags_::kTempoValid as u32
        | StatesAndFlags_::kProjectTimeMusicValid as u32
        | StatesAndFlags_::kBarPositionValid as u32
        | StatesAndFlags_::kTimeSigValid as u32;
    if t.playing {
        c.state |= StatesAndFlags_::kPlaying as u32;
    }
    c.sampleRate = t.sample_rate;
    c.projectTimeSamples = t.position.0;
    c.tempo = t.bpm;
    c.projectTimeMusic = m.beats;
    c.barPositionMusic = m.bar_start;
    c.timeSigNumerator = i32::from(m.numerator);
    c.timeSigDenominator = i32::from(m.denominator);
    if let Some(l) = &t.looping {
        c.state |= StatesAndFlags_::kCycleValid as u32 | StatesAndFlags_::kCycleActive as u32;
        c.cycleStartMusic = l.start_beats;
        c.cycleEndMusic = l.end_beats;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use daw_core::event::EventList;
    use daw_core::node::TransportState;
    use std::path::PathBuf;
    use std::sync::OnceLock;
    use vst3::Steinberg::Vst::IComponentHandlerTrait;

    const FRAMES: u32 = 16;
    const GAIN: ParamID = 0;
    const ACTIVATIONS: ParamID = 2;

    /// The `testgain` example wrapped in a bundle, as `tests/vst3.rs` does.
    #[test]
    fn context_has_bars_beats_and_time_signature() {
        use daw_core::time::{
            Meter, MeterPoint, PPQ, SampleTime, SegmentTempoMap, TickTime, Timeline,
        };
        // Two bars of 4/4 at 120, then 6/8.
        let meter = Meter::new([
            MeterPoint {
                tick: TickTime(0),
                numerator: 4,
                denominator: 4,
            },
            MeterPoint {
                tick: TickTime(8 * PPQ),
                numerator: 6,
                denominator: 8,
            },
        ]);
        let tl = Timeline::new(SegmentTempoMap::new(120.0), meter);
        let t = TransportState::at(&tl, true, SampleTime(300_000), 48_000.0);
        let mut c: VstProcessContext = unsafe { std::mem::zeroed() };
        fill_context(&mut c, &t, 44_100.0);
        assert_eq!(c.sampleRate, 48_000.0);
        assert_eq!(c.projectTimeSamples, 300_000);
        assert_eq!(c.tempo, 120.0);
        assert_eq!(c.projectTimeMusic, 12.5);
        assert_eq!(c.barPositionMusic, 11.0);
        assert_eq!((c.timeSigNumerator, c.timeSigDenominator), (6, 8));
        let valid = StatesAndFlags_::kPlaying as u32
            | StatesAndFlags_::kTempoValid as u32
            | StatesAndFlags_::kProjectTimeMusicValid as u32
            | StatesAndFlags_::kBarPositionValid as u32
            | StatesAndFlags_::kTimeSigValid as u32;
        assert_eq!(c.state & valid, valid);
        assert_eq!(c.state & StatesAndFlags_::kCycleActive as u32, 0);

        let looping = TransportState {
            looping: Some(daw_core::node::TransportLoop::at(
                &tl,
                SampleTime(96_000),
                SampleTime(192_000),
                48_000.0,
            )),
            ..t
        };
        fill_context(&mut c, &looping, 44_100.0);
        let cycle = StatesAndFlags_::kCycleValid as u32 | StatesAndFlags_::kCycleActive as u32;
        assert_eq!(c.state & cycle, cycle);
        assert_eq!((c.cycleStartMusic, c.cycleEndMusic), (4.0, 8.0));

        // No tempo: 120 bpm in 4/4 from sample 0, at the plugin's rate.
        let stopped = TransportState {
            position: SampleTime(88_200),
            ..TransportState::default()
        };
        fill_context(&mut c, &stopped, 44_100.0);
        assert_eq!(
            (c.sampleRate, c.tempo, c.projectTimeMusic),
            (44_100.0, 120.0, 4.0)
        );
        assert_eq!(c.state & StatesAndFlags_::kPlaying as u32, 0);
    }

    fn testgain() -> PluginDescriptor {
        static BUNDLE: OnceLock<PathBuf> = OnceLock::new();
        let bundle = BUNDLE.get_or_init(|| {
            let profile_dir = std::env::current_exe()
                .unwrap()
                .parent()
                .and_then(Path::parent)
                .unwrap()
                .to_path_buf();
            let so = profile_dir.join("examples/libtestgain.so");
            // `cargo test --lib` does not build examples, so build it here, always (a stale
            // copy would test an old plugin), and in this test binary's profile, or the
            // library lands in another profile's directory.
            let profile = match profile_dir.file_name().and_then(|n| n.to_str()) {
                Some("debug") | None => "dev",
                Some(name) => name,
            };
            let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
            let status = std::process::Command::new(cargo)
                .args(["build", "-p", "daw-plugins-vst3", "--example", "testgain"])
                .args(["--profile", profile])
                .status()
                .expect("run cargo to build the test plugin");
            assert!(status.success(), "building the testgain example failed");
            let root = std::env::temp_dir().join(format!("daw-vst3-unit-{}", std::process::id()));
            let bin = root
                .join("TestGain.vst3/Contents")
                .join(format!("{}-linux", std::env::consts::ARCH));
            std::fs::create_dir_all(&bin).unwrap();
            std::fs::copy(&so, bin.join("TestGain.so")).unwrap();
            root.join("TestGain.vst3")
        });
        scan_module(bundle).unwrap().pop().unwrap()
    }

    fn instance() -> Vst3Instance {
        let mut p = Vst3Instance::new(&testgain()).unwrap();
        p.prepare(48_000.0, FRAMES);
        p
    }

    /// What the plugin's editor does when the user moves a control.
    fn editor_edit(p: &Vst3Instance, id: ParamID, normalized: f64) -> i32 {
        unsafe { p.handler.performEdit(id, normalized) }
    }

    fn main_thread(p: &mut Vst3Instance) {
        let _pumping = crate::host::RUN_LOOP_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        p.on_main_thread();
    }

    /// Runs one block of ones through the plugin and returns the left output's first sample.
    fn gain_heard(p: &mut Vst3Instance) -> f32 {
        gain_heard_with(p, &[])
    }

    /// `gain_heard` with events in.
    fn gain_heard_with(p: &mut Vst3Instance, events: &[EventKind]) -> f32 {
        heard_through(&mut |ctx| p.process(ctx), events)
    }

    /// Runs one block of ones through `process` and returns the left output's first sample.
    fn heard_through(process: &mut dyn FnMut(&mut ProcessContext), events: &[EventKind]) -> f32 {
        let input = [1.0f32; FRAMES as usize];
        let mut outs = [[0.0f32; FRAMES as usize]; 2];
        let mut events_in = EventList::with_capacity(8);
        for &kind in events {
            events_in.push(DawEvent { frame: 0, kind }).unwrap();
        }
        let mut events_out = EventList::with_capacity(64);
        let transport = TransportState::default();
        let audio_in: [&[f32]; 2] = [&input, &input];
        let [l, r] = &mut outs;
        let mut audio_out: [&mut [f32]; 2] = [l, r];
        let mut ctx = ProcessContext {
            frames: FRAMES,
            transport: &transport,
            audio_in: &audio_in,
            audio_out: &mut audio_out,
            events_in: &events_in,
            events_out: &mut events_out,
        };
        process(&mut ctx);
        outs[0][0]
    }

    #[test]
    fn save_state_includes_editor_edits_not_yet_processed() {
        let mut p = instance();
        editor_edit(&p, GAIN, 0.3);
        let state = p.save_state().unwrap();

        let mut q = instance();
        q.load_state(&state).unwrap();
        assert_eq!(gain_heard(&mut q), 0.3);
        // The flushed edit is not applied a second time.
        assert_eq!(gain_heard(&mut p), 0.3);
    }

    #[test]
    fn editor_edits_and_dirty_flags_report_a_state_change() {
        use vst3::Steinberg::Vst::IComponentHandler2Trait;
        let mut p = instance();
        assert!(!p.take_state_changed());
        editor_edit(&p, GAIN, 0.3);
        assert!(p.take_state_changed());
        assert!(!p.take_state_changed(), "reported once");
        unsafe { p.handler.setDirty(1) };
        assert!(p.take_state_changed());
        unsafe { p.handler.setDirty(0) };
        assert!(!p.take_state_changed());
        // A preset the plugin loaded itself.
        unsafe {
            p.handler
                .restartComponent(RestartFlags_::kParamValuesChanged as i32)
        };
        main_thread(&mut p);
        assert!(p.take_state_changed());
    }

    #[test]
    fn load_state_is_not_a_change_of_the_plugins_own() {
        let mut p = instance();
        let state = p.save_state().unwrap();
        editor_edit(&p, GAIN, 0.3);
        unsafe {
            p.handler
                .restartComponent(RestartFlags_::kParamValuesChanged as i32)
        };
        p.load_state(&state).unwrap();
        main_thread(&mut p);
        assert!(!p.take_state_changed());
    }

    #[test]
    fn the_last_of_many_editor_edits_wins() {
        let edits = 3 * EDIT_QUEUE;
        let last = |i: usize| i as f64 / edits as f64;

        let mut p = instance();
        for i in 1..=edits {
            assert_eq!(editor_edit(&p, GAIN, last(i)), kResultOk);
        }
        let state = p.save_state().unwrap();
        let mut q = instance();
        q.load_state(&state).unwrap();
        assert_eq!(gain_heard(&mut q), 1.0);

        // And through the audio thread, with the main thread topping the queue up.
        for i in 1..=edits {
            editor_edit(&q, GAIN, last(edits - i));
        }
        for _ in 0..4 {
            gain_heard(&mut q);
            main_thread(&mut q);
        }
        assert_eq!(gain_heard(&mut q), 0.0);
    }

    #[test]
    fn load_state_discards_editor_edits_made_before_it() {
        let mut p = instance();
        let state = p.save_state().unwrap();
        for _ in 0..2 * EDIT_QUEUE {
            editor_edit(&p, GAIN, 0.3);
        }
        p.load_state(&state).unwrap();
        main_thread(&mut p);
        assert_eq!(
            gain_heard(&mut p),
            1.0,
            "the stale edit overrode the loaded state"
        );
    }

    /// Sends the plugin's component a message, as its controller would.
    fn tell_component(p: &Vst3Instance, id: &std::ffi::CStr, ints: &[(&std::ffi::CStr, i64)]) {
        use vst3::Steinberg::Vst::{
            IAttributeListTrait, IHostApplicationTrait, IMessage, IMessageTrait,
        };
        let mut cid: TUID = unsafe { std::mem::transmute(IMessage::IID) };
        let mut iid = cid;
        let mut obj = ptr::null_mut();
        unsafe {
            assert_eq!(
                host().createInstance(&mut cid, &mut iid, &mut obj),
                kResultOk
            );
            let msg = ComPtr::<IMessage>::from_raw(obj as *mut IMessage).unwrap();
            msg.setMessageID(id.as_ptr());
            let attrs = vst3::ComRef::from_raw(msg.getAttributes()).unwrap();
            for (key, value) in ints {
                attrs.setInt(key.as_ptr(), *value);
            }
            let component = &p.connection.as_ref().unwrap().component;
            assert_eq!(component.notify(msg.as_ptr()), kResultOk);
        }
    }

    /// Makes the plugin switch its output to `channels` and report it the way plugins do. A
    /// `locked` plugin refuses to switch back.
    fn switch_outputs(p: &Vst3Instance, channels: i64, locked: bool) {
        tell_component(
            p,
            c"set_outputs",
            &[(c"channels", channels), (c"locked", locked as i64)],
        );
        unsafe { p.handler.restartComponent(RestartFlags_::kIoChanged as i32) };
    }

    #[test]
    fn effects_without_an_event_bus_still_take_parameter_events() {
        let mut p = instance();
        // Most effects have no event bus; testgain has one, so take it away.
        p.buses.event_in = false;
        p.buses.event_out = false;
        p.audio.main(|a| a.event_in = false);
        assert!(p.ports().event_in, "the engine would refuse automation");
        assert!(p.ports().event_out, "editor edits could not be recorded");
        let heard = gain_heard_with(
            &mut p,
            &[
                EventKind::ParamValue {
                    param: GAIN,
                    value: 0.5,
                },
                EventKind::NoteOn {
                    channel: 0,
                    key: 60,
                    velocity: 1.0,
                    note_id: -1,
                },
            ],
        );
        assert_eq!(heard, 0.5);
        let notes_sent = p
            .audio
            .main(|a| a.rt.as_ref().unwrap().in_events.iter().next().is_some());
        assert!(!notes_sent);

        // A plugin that gains its first parameter (or loses its last) changes its event
        // ports, which the host must hear about.
        p.has_params = false;
        unsafe {
            p.handler
                .restartComponent(RestartFlags_::kParamTitlesChanged as i32)
        };
        main_thread(&mut p);
        assert!(p.take_layout_changed());
    }

    #[test]
    fn corrupt_midi_data_bytes_stay_in_range() {
        let mut p = instance();
        // CC 7 is mapped to the gain; 0xff is not a valid data byte.
        let heard = gain_heard_with(&mut p, &[EventKind::Midi([0xb0, 7, 0xff])]);
        assert!((0.0..=1.0).contains(&heard), "gain {heard} out of range");
    }

    #[test]
    fn the_controller_sees_the_newest_value_however_long_the_main_thread_takes() {
        let mut p = instance();
        // Enough blocks to fill any fixed queue many times over (each block also reports
        // two read-only parameters) before the main thread runs.
        let blocks = 4 * EDIT_QUEUE;
        for i in 0..blocks {
            let value = i as f64 / blocks as f64;
            gain_heard_with(&mut p, &[EventKind::ParamValue { param: GAIN, value }]);
        }
        gain_heard_with(
            &mut p,
            &[EventKind::ParamValue {
                param: GAIN,
                value: 0.25,
            }],
        );
        assert_eq!(p.param_value(GAIN), 0.25);
    }

    #[test]
    fn messages_sent_from_process_never_run_controller_code_on_the_audio_thread() {
        const OFF_THREAD: ParamID = 3;
        let mut p = instance();
        tell_component(&p, c"meter_on", &[]);
        // The engine runs `process` on its own thread.
        std::thread::scope(|s| {
            s.spawn(|| gain_heard(&mut p));
        });
        main_thread(&mut p);
        assert_eq!(
            p.param_value(OFF_THREAD),
            0.0,
            "the controller ran on the audio thread"
        );
    }

    #[test]
    fn io_changes_keep_the_engines_layout_when_the_plugin_agrees() {
        let mut p = instance();
        switch_outputs(&p, 1, false);
        main_thread(&mut p);
        assert_eq!(p.ports().audio_out, 2, "ports changed behind the engine");
        assert!(p.take_layout_changed());
        // The plugin took the stereo layout back, so it keeps playing without a prepare.
        assert_eq!(gain_heard(&mut p), 1.0);
        p.prepare(48_000.0, FRAMES);
        assert_eq!(p.ports().audio_out, 2);
    }

    #[test]
    fn io_changes_wait_for_the_host_to_prepare_again() {
        let mut p = instance();
        assert_eq!(p.ports().audio_out, 2);
        switch_outputs(&p, 1, true);
        main_thread(&mut p);

        // The compiled graph still has two output channels wired, so the layout must not
        // change under it, and the plugin must not run on buffers it no longer agrees with.
        assert_eq!(p.ports().audio_out, 2, "ports changed behind the engine");
        assert!(p.take_layout_changed());
        assert!(!p.take_layout_changed(), "reported twice");
        assert_eq!(gain_heard(&mut p), 0.0);

        p.prepare(48_000.0, FRAMES);
        assert_eq!(p.ports().audio_out, 1);
        assert!(!p.take_layout_changed());
        assert_eq!(gain_heard(&mut p), 1.0);
    }

    #[test]
    fn latency_changes_are_reported_to_the_host() {
        let mut p = instance();
        unsafe {
            p.handler
                .restartComponent(RestartFlags_::kLatencyChanged as i32)
        };
        main_thread(&mut p);
        assert!(p.take_layout_changed());
        assert_eq!(p.latency(), 32);
        assert_eq!(
            gain_heard(&mut p),
            1.0,
            "a latency change silenced the plugin"
        );
        // The deactivate/activate cycle that reads the new latency waits for `prepare`.
        main_thread(&mut p);
        assert_eq!(
            p.param_value(ACTIVATIONS),
            0.01,
            "reactivated behind the engine"
        );
        p.prepare(48_000.0, FRAMES);
        gain_heard(&mut p);
        main_thread(&mut p);
        assert_eq!(p.param_value(ACTIVATIONS), 0.02);
    }

    #[test]
    fn a_split_instance_processes_while_the_main_thread_works() {
        let mut p = instance();
        let mut half = p.split().unwrap();
        assert!(p.split().is_none(), "split twice");
        // The instance's own `process` does nothing while the half is out.
        assert_eq!(gain_heard(&mut p), 0.0);

        let done = AtomicBool::new(false);
        let blocks = std::thread::scope(|s| {
            let audio = s.spawn(|| {
                let mut blocks = Vec::new();
                while !done.load(Ordering::Acquire) || blocks.len() < 200 {
                    let value = if blocks.len() < 100 { 1.0 } else { 0.5 };
                    let param = EventKind::ParamValue { param: GAIN, value };
                    blocks.push(heard_through(&mut |ctx| half.process(ctx), &[param]));
                }
                blocks
            });
            // Main-thread calls that VST3 allows during processing never hold the audio
            // half up, so no block above comes out silent.
            for _ in 0..200 {
                main_thread(&mut p);
                p.param_value(GAIN);
                p.has_editor();
                p.save_state().unwrap();
            }
            done.store(true, Ordering::Release);
            audio.join().unwrap()
        });
        assert!(
            blocks.iter().all(|&g| g == 1.0 || g == 0.5),
            "a block went silent: {blocks:?}"
        );
        main_thread(&mut p);
        assert_eq!(
            p.param_value(GAIN),
            0.5,
            "the controller missed the half's values"
        );

        p.join(half);
        assert_eq!(gain_heard(&mut p), 0.5);
        p.prepare(48_000.0, FRAMES);
        assert_eq!(gain_heard(&mut p), 0.5);
        assert!(p.split().is_some(), "split again after join");
    }

    #[test]
    fn editor_edits_and_restarts_reach_a_split_instance() {
        let mut p = instance();
        let mut half = p.split().unwrap();
        let heard =
            |half: &mut Box<dyn PluginProcessor>| heard_through(&mut |ctx| half.process(ctx), &[]);

        editor_edit(&p, GAIN, 0.25);
        // Saving with the half out flushes the edit through it, under the audio lock.
        let state = p.save_state().unwrap();
        let mut q = instance();
        q.load_state(&state).unwrap();
        assert_eq!(gain_heard(&mut q), 0.25);
        assert_eq!(heard(&mut half), 0.25);

        editor_edit(&p, GAIN, 0.75);
        assert_eq!(heard(&mut half), 0.75);

        // A layout change the plugin takes back keeps the half playing; one it refuses
        // silences it until the host joins and prepares.
        switch_outputs(&p, 1, false);
        main_thread(&mut p);
        assert!(p.take_layout_changed());
        assert_eq!(heard(&mut half), 0.75);
        switch_outputs(&p, 1, true);
        main_thread(&mut p);
        assert!(p.take_layout_changed());
        assert_eq!(heard(&mut half), 0.0);

        // A new parameter list reaches the half too.
        unsafe {
            p.handler
                .restartComponent(RestartFlags_::kParamTitlesChanged as i32)
        };
        main_thread(&mut p);
        p.join(half);
        p.prepare(48_000.0, FRAMES);
        assert_eq!(p.ports().audio_out, 1);
        let mut half = p.split().unwrap();
        let param = EventKind::ParamValue {
            param: GAIN,
            value: 0.5,
        };
        assert_eq!(heard_through(&mut |ctx| half.process(ctx), &[param]), 0.5);
        main_thread(&mut p);
        assert_eq!(p.param_value(GAIN), 0.5);
        p.join(half);
    }

    #[test]
    fn load_state_discards_editor_edits_before_a_split_half_hears_them() {
        let mut p = instance();
        let state = p.save_state().unwrap();
        let mut half = p.split().unwrap();
        for _ in 0..2 * EDIT_QUEUE {
            editor_edit(&p, GAIN, 0.3);
        }
        p.load_state(&state).unwrap();
        main_thread(&mut p);
        assert_eq!(
            heard_through(&mut |ctx| half.process(ctx), &[]),
            1.0,
            "the stale edit overrode the loaded state"
        );
        p.join(half);
    }

    #[test]
    fn nothing_splits_before_the_first_prepare() {
        let mut p = Vst3Instance::new(&testgain()).unwrap();
        assert!(p.split().is_none());
        p.prepare(48_000.0, FRAMES);
        let half = p.split().unwrap();
        p.join(half);
    }

    #[test]
    fn a_processor_from_another_instance_is_not_taken_back() {
        let mut p = instance();
        let mut q = instance();
        let _ = p.split().unwrap();
        q.join(Box::new(Vst3Processor(Arc::clone(&p.audio))));
        assert!(q.split().is_some(), "q's own half was never out");
        assert!(p.split().is_none(), "p's half is still out");
        p.lent = false;
    }

    #[test]
    fn midi_notes_and_pressure_convert() {
        let on = midi_to_vst_event(3, [0x91, 60, 127]).unwrap();
        assert_eq!(on.r#type, EventTypes_::kNoteOnEvent as u16);
        assert_eq!(on.sampleOffset, 3);
        let n = unsafe { on.__field0.noteOn };
        assert_eq!((n.channel, n.pitch, n.velocity), (1, 60, 1.0));

        let off = midi_to_vst_event(0, [0x90, 60, 0]).unwrap();
        assert_eq!(off.r#type, EventTypes_::kNoteOffEvent as u16);
        assert!(midi_to_vst_event(0, [0xa0, 1, 2]).is_some());
        assert!(midi_to_vst_event(0, [0xb0, 1, 2]).is_none());
    }

    fn info() -> ParamInfo {
        ParamInfo {
            id: 0,
            name: String::new(),
            min: 0.0,
            max: 0.0,
            default: 0.0,
            automatable: true,
        }
    }

    #[test]
    fn plain_values_map_linearly() {
        let p = Param::sampled(info(), 0, |n| -12.0 + 24.0 * n);
        assert_eq!(p.to_normalized(0.0), 0.5);
        assert_eq!(p.to_plain(0.25), -6.0);
        assert_eq!(p.to_normalized(100.0), 1.0);
        assert_eq!(p.to_normalized(-100.0), 0.0);
    }

    #[test]
    fn plain_values_follow_the_plugins_curve() {
        // A 20 Hz to 20 kHz cutoff on a log scale.
        let p = Param::sampled(info(), 0, |n| 20.0 * 1000f64.powf(n));
        let expected = 50f64.ln() / 1000f64.ln();
        assert!((p.to_normalized(1000.0) - expected).abs() < 1e-3);
        assert!((p.to_plain(0.5) / 1000f64.sqrt() / 20.0 - 1.0).abs() < 1e-3);
        assert_eq!((p.to_plain(0.0), p.to_plain(1.0)), (20.0, 20_000.0));
        for n in [0.0, 0.1, 0.37, 0.9, 1.0] {
            assert!((p.to_normalized(p.to_plain(n)) - n).abs() < 1e-9);
        }

        // Falling curves work too.
        let p = Param::sampled(info(), 0, |n| 1.0 - n * n);
        assert!((p.to_normalized(0.75) - 0.5).abs() < 1e-3);

        // Discrete parameters map each step onto the plugin's normalized value for it.
        let p = Param::sampled(info(), 3, |n| (n * 4.0).floor().min(3.0));
        assert_eq!(p.to_normalized(2.0), 2.0 / 3.0);
        assert_eq!(p.to_plain(1.0 / 3.0), 1.0);
    }
}
