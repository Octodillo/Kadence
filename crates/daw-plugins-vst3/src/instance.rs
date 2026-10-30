//! A running VST3 plugin: component, edit controller and audio processor behind
//! [`PluginInstance`].

use std::collections::HashMap;
use std::ffi::c_void;
use std::path::Path;
use std::ptr;
use std::sync::{Arc, Mutex};

use daw_core::event::{Event as DawEvent, EventKind, ParamId};
use daw_core::node::{Node, PortLayout, ProcessContext};
use daw_core::plugin::{
    ParamInfo, PluginDescriptor, PluginError, PluginFormatKind, PluginInstance,
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
    ComponentHandler, Edit, EventQueue, MemoryStream, ParamChanges, host, host_context,
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

pub struct Vst3Instance {
    descriptor: PluginDescriptor,
    editor: Option<Editor>,
    rt: Option<Box<Rt>>,
    params: Vec<Param>,
    param_index: HashMap<ParamID, usize>,
    /// `[channel][controller]` to parameter, or `kNoParamId`.
    midi_map: Box<[[ParamID; MIDI_CONTROLLERS]; 16]>,
    buses: Buses,
    sample_rate: f64,
    max_frames: u32,
    latency: u32,
    active: bool,
    processing: bool,

    /// Edits from the plugin's editor, read by `process`.
    edits_rx: rtrb::Consumer<Edit>,
    /// Values the processor saw, forwarded to the controller off the real-time thread.
    to_controller_tx: rtrb::Producer<Edit>,
    to_controller_rx: Mutex<rtrb::Consumer<Edit>>,

    connection: Option<(ComPtr<IConnectionPoint>, ComPtr<IConnectionPoint>)>,
    processor: ComPtr<IAudioProcessor>,
    controller: ComPtr<IEditController>,
    /// False when the component implements the controller itself.
    split: bool,
    component: ComPtr<IComponent>,
    handler: ComWrapper<ComponentHandler>,
    // Dropped last so the plugin's code stays mapped until every object above is released.
    _module: Arc<Module>,
}

// SAFETY: the raw pointers in `Rt` only point at buffers owned by the same instance, and the
// engine never uses an instance from two threads at once (every method takes `&mut self`
// except the read-only `param_value`, which goes through a mutex).
unsafe impl Send for Vst3Instance {}

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

/// Lists the processor classes of one module as descriptors.
pub fn scan_module(path: &Path) -> Vec<PluginDescriptor> {
    let module = match Module::load(path) {
        Ok(m) => m,
        Err(e) => {
            log::warn!("vst3 scan: {e}");
            return Vec::new();
        }
    };
    let factory_vendor = module.factory_vendor();
    module
        .classes()
        .into_iter()
        .filter(|c| c.category == AUDIO_EFFECT_CLASS)
        .map(|class| {
            let ports = probe_ports(&module, &class).unwrap_or((0, 0));
            descriptor_for(path, &class, &factory_vendor, ports)
        })
        .collect()
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
        let (to_controller_tx, to_controller_rx) = rtrb::RingBuffer::new(EDIT_QUEUE);
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
                    (Some(a), Some(b)) => {
                        a.connect(b.as_ptr());
                        b.connect(a.as_ptr());
                        Some((a, b))
                    }
                    _ => None,
                }
            } else {
                None
            };

            if let Some(h) = handler.to_com_ptr::<IComponentHandler>() {
                controller.setComponentHandler(h.as_ptr());
            }

            let mut instance = Vst3Instance {
                descriptor: d.clone(),
                editor: None,
                rt: None,
                params: Vec::new(),
                param_index: HashMap::new(),
                midi_map: Box::new([[kNoParamId; MIDI_CONTROLLERS]; 16]),
                buses: read_buses(&component),
                sample_rate: 0.0,
                max_frames: 0,
                latency: 0,
                active: false,
                processing: false,
                edits_rx,
                to_controller_tx,
                to_controller_rx: Mutex::new(to_controller_rx),
                connection,
                processor,
                controller,
                split,
                component,
                handler,
                _module: module,
            };
            instance.sync_controller_to_component();
            instance.read_params();
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

    fn read_params(&mut self) {
        let mut params = Vec::new();
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
                params.push(param);
            }
        }
        self.param_index = params
            .iter()
            .enumerate()
            .map(|(i, p)| (p.info.id, i))
            .collect();
        self.params = params;
    }

    fn read_midi_map(&mut self) {
        let Some(mapping) = self.controller.cast::<IMidiMapping>() else {
            return;
        };
        for (channel, row) in self.midi_map.iter_mut().enumerate() {
            for (cc, slot) in row.iter_mut().enumerate() {
                let mut id: ParamID = kNoParamId;
                let found = unsafe {
                    mapping.getMidiControllerAssignment(0, channel as i16, cc as i16, &mut id)
                };
                *slot = if found == kResultOk || found == kResultTrue {
                    id
                } else {
                    kNoParamId
                };
            }
        }
    }

    fn deactivate(&mut self) {
        unsafe {
            if self.processing {
                self.processor.setProcessing(0);
                self.processing = false;
            }
            if self.active {
                self.component.setActive(0);
                self.active = false;
            }
        }
    }

    fn activate(&mut self) {
        unsafe {
            if !self.active {
                self.component.setActive(1);
                self.active = true;
            }
            self.latency = self.processor.getLatencySamples();
            if !self.processing {
                // Plugins may refuse; they still process, so carry on either way.
                self.processor.setProcessing(1);
                self.processing = true;
            }
        }
    }

    fn param(&self, id: ParamID) -> Option<&Param> {
        self.param_index.get(&id).map(|&i| &self.params[i])
    }

    fn to_plain(&self, id: ParamID, normalized: f64) -> f64 {
        self.param(id)
            .map_or(normalized, |p| p.to_plain(normalized))
    }

    /// Applies processor-side values to the controller so editors and `param_value` follow.
    fn flush_to_controller(&self) {
        let mut rx = self
            .to_controller_rx
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        while let Ok(e) = rx.pop() {
            unsafe { self.controller.setParamNormalized(e.id, e.normalized) };
        }
    }

    /// Hands editor edits still queued for the processor over with a zero-sample `process`,
    /// the spec's parameter flush, so a state saved while the engine is not running this
    /// instance includes them. `&mut self` means the audio thread is not using it. Edits
    /// flushed this way are not reported to the host as `ParamValue` events.
    fn flush_edits(&mut self) {
        if !self.processing {
            return;
        }
        let Some(rt) = self.rt.as_deref_mut() else {
            return;
        };
        // Bounded, so a plugin that calls `performEdit` from its own `process` cannot keep
        // the loop going forever. Each round empties the ring and refills it from overflow,
        // which holds at most one edit per parameter.
        for _ in 0..FLUSH_ROUNDS {
            self.handler.retry_overflow();
            if self.edits_rx.is_empty() {
                return;
            }
            rt.in_params.clear();
            rt.out_params.clear();
            rt.in_events.clear();
            rt.out_events.clear();
            while let Ok(e) = self.edits_rx.pop() {
                rt.in_params.add(e.id, 0, e.normalized);
            }
            let mut data = rt.process_data(0);
            unsafe { self.processor.process(&mut data) };
            for q in rt.out_params.queues() {
                if let Some((_, normalized)) = q.last() {
                    let _ = self.to_controller_tx.push(Edit {
                        id: q.id(),
                        normalized,
                    });
                }
            }
        }
    }

    fn handle_restart(&mut self) {
        let flags = self.handler.take_restart_flags();
        if flags == 0 {
            return;
        }
        if flags & (RestartFlags_::kIoChanged as i32) != 0 && self.rt.is_some() {
            self.buses = read_buses(&self.component);
            let (sr, frames) = (self.sample_rate, self.max_frames);
            self.prepare(sr, frames);
        } else if flags & (RestartFlags_::kLatencyChanged as i32) != 0 && self.active {
            // The spec asks for a deactivate/activate cycle before reading the new latency.
            self.deactivate();
            self.activate();
        }
        if flags
            & (RestartFlags_::kParamValuesChanged as i32
                | RestartFlags_::kParamTitlesChanged as i32)
            != 0
        {
            self.read_params();
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
            event_in: self.buses.event_in,
            event_out: self.buses.event_out,
        }
    }

    fn prepare(&mut self, sample_rate: f64, max_frames: u32) {
        self.deactivate();
        self.sample_rate = sample_rate;
        self.max_frames = max_frames.max(1);
        unsafe {
            let mut setup = ProcessSetup {
                processMode: ProcessModes_::kRealtime as i32,
                symbolicSampleSize: SymbolicSampleSizes_::kSample32 as i32,
                maxSamplesPerBlock: self.max_frames as i32,
                sampleRate: sample_rate,
            };
            self.processor.setupProcessing(&mut setup);

            // Offer the plugin its own default arrangements, then use whatever it ends up with.
            let arrangements = |dir: i32, n: usize| -> Vec<SpeakerArrangement> {
                (0..n)
                    .map(|i| {
                        let mut arr: SpeakerArrangement = 0;
                        self.processor.getBusArrangement(dir, i as i32, &mut arr);
                        arr
                    })
                    .collect()
            };
            let mut ins = arrangements(BusDirections_::kInput as i32, self.buses.inputs.len());
            let mut outs = arrangements(BusDirections_::kOutput as i32, self.buses.outputs.len());
            self.processor.setBusArrangements(
                ins.as_mut_ptr(),
                ins.len() as i32,
                outs.as_mut_ptr(),
                outs.len() as i32,
            );
            self.buses = read_buses(&self.component);

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
        self.rt = Some(Rt::new(&self.buses, self.max_frames, sample_rate));
        self.activate();
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        let Some(rt) = self.rt.as_deref_mut() else {
            for out in ctx.audio_out.iter_mut() {
                out.fill(0.0);
            }
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
            let value = self
                .param_index
                .get(&e.id)
                .map_or(e.normalized, |&i| self.params[i].to_plain(e.normalized));
            let _ = ctx.events_out.push(DawEvent {
                frame: 0,
                kind: EventKind::ParamValue { param: e.id, value },
            });
        }

        for ev in ctx.events_in.iter() {
            let frame = ev.frame.min(frames.saturating_sub(1) as u32);
            match ev.kind {
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
                    key,
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
                        0xb0 => (bytes[1] as usize & 0x7f, bytes[2] as f64 / 127.0),
                        0xd0 => (
                            ControllerNumbers_::kAfterTouch as usize,
                            bytes[1] as f64 / 127.0,
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
                        let _ = self.to_controller_tx.push(Edit { id, normalized });
                    }
                }
                EventKind::ParamValue { param, value } => {
                    let normalized = self
                        .param_index
                        .get(&param)
                        .map_or(value.clamp(0.0, 1.0), |&i| {
                            self.params[i].to_normalized(value)
                        });
                    rt.in_params.add(param, frame as i32, normalized);
                    let _ = self.to_controller_tx.push(Edit {
                        id: param,
                        normalized,
                    });
                }
            }
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

        let t = ctx.transport;
        let sample_rate = if t.sample_rate > 0.0 {
            t.sample_rate
        } else {
            self.sample_rate
        };
        let c = &mut rt.context;
        c.state = StatesAndFlags_::kTempoValid as u32
            | StatesAndFlags_::kProjectTimeMusicValid as u32
            | StatesAndFlags_::kTimeSigValid as u32
            | StatesAndFlags_::kContTimeValid as u32;
        if t.playing {
            c.state |= StatesAndFlags_::kPlaying as u32;
        }
        c.sampleRate = sample_rate;
        c.projectTimeSamples = t.position.0;
        c.tempo = if t.bpm > 0.0 { t.bpm } else { 120.0 };
        c.projectTimeMusic = t.position.0 as f64 / sample_rate * c.tempo / 60.0;

        let mut data = rt.process_data(frames as i32);
        unsafe { self.processor.process(&mut data) };
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
            let _ = self.to_controller_tx.push(Edit { id, normalized });
            let value = self
                .param_index
                .get(&id)
                .map_or(normalized, |&i| self.params[i].to_plain(normalized));
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
                            key: n.pitch as u8,
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

    fn latency(&self) -> u32 {
        self.latency
    }

    /// Runs on the audio thread, so only cycles `setProcessing`, which the spec allows there
    /// and plugins treat as a state reset. `setActive` and the latency re-read stay on the
    /// main thread, in `prepare` and `handle_restart`.
    fn reset(&mut self) {
        if self.processing {
            unsafe {
                self.processor.setProcessing(0);
                self.processor.setProcessing(1);
            }
        }
    }
}

impl PluginInstance for Vst3Instance {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    fn params(&self) -> Vec<ParamInfo> {
        self.params.iter().map(|p| p.info.clone()).collect()
    }

    fn param_value(&self, id: ParamId) -> f64 {
        self.flush_to_controller();
        let normalized = unsafe { self.controller.getParamNormalized(id) };
        self.to_plain(id, normalized)
    }

    /// Format: magic, version, then the component and controller states as length-prefixed
    /// blobs (little endian).
    fn save_state(&mut self) -> Result<Vec<u8>, PluginError> {
        self.flush_edits();
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
        // Values pushed before the load are stale now, in both directions: editor edits still
        // queued for the processor would otherwise overwrite the restored state.
        self.handler.discard_overflow();
        while self.edits_rx.pop().is_ok() {}
        let mut rx = self
            .to_controller_rx
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        while rx.pop().is_ok() {}
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
        let editor =
            Editor::open(&self.controller, &self.descriptor.name).map_err(PluginError::Editor)?;
        self.editor = Some(editor);
        Ok(())
    }

    fn close_editor(&mut self) {
        self.editor = None;
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
}

impl Drop for Vst3Instance {
    fn drop(&mut self) {
        self.editor = None;
        self.deactivate();
        self.rt = None;
        unsafe {
            if let Some((a, b)) = self.connection.take() {
                a.disconnect(b.as_ptr());
                b.disconnect(a.as_ptr());
            }
            self.controller.setComponentHandler(ptr::null_mut());
            if self.split {
                self.controller.terminate();
            }
            self.component.terminate();
        }
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

    /// The `testgain` example wrapped in a bundle, as `tests/vst3.rs` does.
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
        scan_module(bundle).pop().unwrap()
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
        let input = [1.0f32; FRAMES as usize];
        let mut outs = [[0.0f32; FRAMES as usize]; 2];
        let events_in = EventList::with_capacity(8);
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
        p.process(&mut ctx);
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
