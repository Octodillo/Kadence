//! A tiny split processor/controller VST3 used by this crate's tests.
//!
//! - Stereo in and out, multiplied by parameter 0 ("Gain", plain = normalized, 0..1).
//! - Parameter 1 ("Peak") is read only; the processor reports the output peak through it.
//! - Note events are echoed to the event output.
//! - Reports 32 samples of latency.
//! - Component state is the gain as a little-endian f64.
//! - MIDI CC 7 is mapped to the gain.

#![allow(non_snake_case, clippy::unnecessary_cast)]

use std::ffi::{c_char, c_void};
use std::slice;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use vst3::Steinberg::Vst::*;
use vst3::Steinberg::*;
use vst3::{Class, ComPtr, ComRef, ComWrapper, uid};

const NAME: &str = "Test Gain";
const LATENCY: u32 = 32;
const GAIN: ParamID = 0;
const PEAK: ParamID = 1;

fn copy_cstr(src: &str, dst: &mut [c_char]) {
    for (d, s) in dst.iter_mut().zip(src.bytes().chain(std::iter::once(0))) {
        *d = s as c_char;
    }
}

fn copy_wstr(src: &str, dst: &mut [TChar]) {
    for (d, s) in dst
        .iter_mut()
        .zip(src.encode_utf16().chain(std::iter::once(0)))
    {
        *d = s as TChar;
    }
}

unsafe fn read_f64(stream: *mut IBStream) -> Option<f64> {
    let stream = unsafe { ComRef::from_raw(stream) }?;
    let mut bytes = [0u8; 8];
    let mut n = 0;
    unsafe { stream.read(bytes.as_mut_ptr().cast(), 8, &mut n) };
    (n == 8).then(|| f64::from_le_bytes(bytes))
}

unsafe fn write_f64(stream: *mut IBStream, v: f64) -> tresult {
    let Some(stream) = (unsafe { ComRef::from_raw(stream) }) else {
        return kInvalidArgument;
    };
    let mut bytes = v.to_le_bytes();
    let mut n = 0;
    unsafe { stream.write(bytes.as_mut_ptr().cast(), 8, &mut n) };
    if n == 8 { kResultOk } else { kResultFalse }
}

struct Processor {
    gain: AtomicU64,
    peer: Mutex<Option<ComPtr<IConnectionPoint>>>,
}

impl Class for Processor {
    type Interfaces = (IComponent, IAudioProcessor, IConnectionPoint);
}

impl Processor {
    const CID: TUID = uid(0x8E2A4C11, 0x5B3D4F60, 0x9A7E21C4, 0xD0F3B512);

    fn gain(&self) -> f64 {
        f64::from_bits(self.gain.load(Ordering::Relaxed))
    }
}

impl IPluginBaseTrait for Processor {
    unsafe fn initialize(&self, _context: *mut FUnknown) -> tresult {
        kResultOk
    }

    unsafe fn terminate(&self) -> tresult {
        kResultOk
    }
}

impl IConnectionPointTrait for Processor {
    unsafe fn connect(&self, other: *mut IConnectionPoint) -> tresult {
        *self.peer.lock().unwrap() = unsafe { ComRef::from_raw(other) }.map(|r| r.to_com_ptr());
        kResultOk
    }

    unsafe fn disconnect(&self, _other: *mut IConnectionPoint) -> tresult {
        *self.peer.lock().unwrap() = None;
        kResultOk
    }

    unsafe fn notify(&self, _message: *mut IMessage) -> tresult {
        kResultOk
    }
}

impl IComponentTrait for Processor {
    unsafe fn getControllerClassId(&self, class_id: *mut TUID) -> tresult {
        unsafe { *class_id = Controller::CID };
        kResultOk
    }

    unsafe fn setIoMode(&self, _mode: IoMode) -> tresult {
        kResultOk
    }

    unsafe fn getBusCount(&self, _media: MediaType, _dir: BusDirection) -> i32 {
        1
    }

    unsafe fn getBusInfo(
        &self,
        media: MediaType,
        dir: BusDirection,
        index: i32,
        bus: *mut BusInfo,
    ) -> tresult {
        if index != 0 {
            return kInvalidArgument;
        }
        let bus = unsafe { &mut *bus };
        bus.mediaType = media;
        bus.direction = dir;
        bus.busType = BusTypes_::kMain as BusType;
        bus.flags = BusInfo_::BusFlags_::kDefaultActive as u32;
        if media == MediaTypes_::kAudio as MediaType {
            bus.channelCount = 2;
            copy_wstr("Main", &mut bus.name);
        } else {
            bus.channelCount = 16;
            copy_wstr("Notes", &mut bus.name);
        }
        kResultOk
    }

    unsafe fn getRoutingInfo(&self, _in: *mut RoutingInfo, _out: *mut RoutingInfo) -> tresult {
        kNotImplemented
    }

    unsafe fn activateBus(&self, _m: MediaType, _d: BusDirection, _i: i32, _s: TBool) -> tresult {
        kResultOk
    }

    unsafe fn setActive(&self, _state: TBool) -> tresult {
        kResultOk
    }

    unsafe fn setState(&self, state: *mut IBStream) -> tresult {
        match unsafe { read_f64(state) } {
            Some(g) => {
                self.gain.store(g.to_bits(), Ordering::Relaxed);
                kResultOk
            }
            None => kResultFalse,
        }
    }

    unsafe fn getState(&self, state: *mut IBStream) -> tresult {
        unsafe { write_f64(state, self.gain()) }
    }
}

impl IAudioProcessorTrait for Processor {
    unsafe fn setBusArrangements(
        &self,
        inputs: *mut SpeakerArrangement,
        num_ins: i32,
        outputs: *mut SpeakerArrangement,
        num_outs: i32,
    ) -> tresult {
        let ok = num_ins == 1
            && num_outs == 1
            && unsafe { *inputs == SpeakerArr::kStereo && *outputs == SpeakerArr::kStereo };
        if ok { kResultTrue } else { kResultFalse }
    }

    unsafe fn getBusArrangement(
        &self,
        _d: BusDirection,
        index: i32,
        arr: *mut SpeakerArrangement,
    ) -> tresult {
        if index != 0 {
            return kInvalidArgument;
        }
        unsafe { *arr = SpeakerArr::kStereo };
        kResultOk
    }

    unsafe fn canProcessSampleSize(&self, size: i32) -> tresult {
        if size == SymbolicSampleSizes_::kSample32 as i32 {
            kResultOk
        } else {
            kNotImplemented
        }
    }

    unsafe fn getLatencySamples(&self) -> u32 {
        LATENCY
    }

    unsafe fn setupProcessing(&self, _setup: *mut ProcessSetup) -> tresult {
        kResultOk
    }

    unsafe fn setProcessing(&self, _state: TBool) -> tresult {
        kResultOk
    }

    unsafe fn process(&self, data: *mut ProcessData) -> tresult {
        let data = unsafe { &*data };

        if let Some(changes) = unsafe { ComRef::from_raw(data.inputParameterChanges) } {
            for i in 0..unsafe { changes.getParameterCount() } {
                let Some(q) = (unsafe { ComRef::from_raw(changes.getParameterData(i)) }) else {
                    continue;
                };
                if unsafe { q.getParameterId() } != GAIN {
                    continue;
                }
                let (mut offset, mut value) = (0, 0.0);
                let last = unsafe { q.getPointCount() } - 1;
                if unsafe { q.getPoint(last, &mut offset, &mut value) } == kResultTrue {
                    self.gain.store(value.to_bits(), Ordering::Relaxed);
                }
            }
        }

        if let (Some(input), Some(output)) = unsafe {
            (
                ComRef::from_raw(data.inputEvents),
                ComRef::from_raw(data.outputEvents),
            )
        } {
            for i in 0..unsafe { input.getEventCount() } {
                let mut e: Event = unsafe { std::mem::zeroed() };
                if unsafe { input.getEvent(i, &mut e) } == kResultOk {
                    unsafe { output.addEvent(&mut e) };
                }
            }
        }

        let gain = self.gain() as f32;
        let n = data.numSamples as usize;
        let mut peak = 0.0f32;
        if data.numInputs == 1 && data.numOutputs == 1 {
            unsafe {
                let ins = slice::from_raw_parts(data.inputs, 1)[0];
                let outs = slice::from_raw_parts(data.outputs, 1)[0];
                let ins =
                    slice::from_raw_parts(ins.__field0.channelBuffers32, ins.numChannels as usize);
                let outs = slice::from_raw_parts(
                    outs.__field0.channelBuffers32,
                    outs.numChannels as usize,
                );
                for (i, o) in ins.iter().zip(outs) {
                    let i = slice::from_raw_parts(*i, n);
                    let o = slice::from_raw_parts_mut(*o, n);
                    for (x, y) in i.iter().zip(o.iter_mut()) {
                        *y = x * gain;
                        peak = peak.max(y.abs());
                    }
                }
            }
        }

        if let Some(out) = unsafe { ComRef::from_raw(data.outputParameterChanges) } {
            let mut index = 0;
            if let Some(q) = unsafe { ComRef::from_raw(out.addParameterData(&PEAK, &mut index)) } {
                unsafe { q.addPoint(0, peak.min(1.0) as f64, &mut index) };
            }
        }
        kResultOk
    }

    unsafe fn getTailSamples(&self) -> u32 {
        0
    }
}

struct Controller {
    values: Mutex<[f64; 2]>,
    handler: Mutex<Option<ComPtr<IComponentHandler>>>,
}

impl Class for Controller {
    type Interfaces = (IEditController, IMidiMapping, IConnectionPoint);
}

impl Controller {
    const CID: TUID = uid(0x4F61D2A7, 0x1C8B4E39, 0xB25A6E0D, 0x7C94F3A8);
}

impl IPluginBaseTrait for Controller {
    unsafe fn initialize(&self, _context: *mut FUnknown) -> tresult {
        kResultOk
    }

    unsafe fn terminate(&self) -> tresult {
        kResultOk
    }
}

impl IConnectionPointTrait for Controller {
    unsafe fn connect(&self, _other: *mut IConnectionPoint) -> tresult {
        kResultOk
    }

    unsafe fn disconnect(&self, _other: *mut IConnectionPoint) -> tresult {
        kResultOk
    }

    unsafe fn notify(&self, _message: *mut IMessage) -> tresult {
        kResultOk
    }
}

impl IMidiMappingTrait for Controller {
    unsafe fn getMidiControllerAssignment(
        &self,
        bus: i32,
        _channel: i16,
        cc: CtrlNumber,
        id: *mut ParamID,
    ) -> tresult {
        if bus == 0 && cc == ControllerNumbers_::kCtrlVolume as CtrlNumber {
            unsafe { *id = GAIN };
            kResultTrue
        } else {
            kResultFalse
        }
    }
}

impl IEditControllerTrait for Controller {
    unsafe fn setComponentState(&self, state: *mut IBStream) -> tresult {
        match unsafe { read_f64(state) } {
            Some(g) => {
                self.values.lock().unwrap()[GAIN as usize] = g;
                kResultOk
            }
            None => kResultFalse,
        }
    }

    unsafe fn setState(&self, _state: *mut IBStream) -> tresult {
        kResultOk
    }

    unsafe fn getState(&self, _state: *mut IBStream) -> tresult {
        kResultOk
    }

    unsafe fn getParameterCount(&self) -> i32 {
        2
    }

    unsafe fn getParameterInfo(&self, index: i32, info: *mut ParameterInfo) -> tresult {
        let info = unsafe { &mut *info };
        let (title, flags) = match index as u32 {
            GAIN => ("Gain", ParameterInfo_::ParameterFlags_::kCanAutomate),
            PEAK => ("Peak", ParameterInfo_::ParameterFlags_::kIsReadOnly),
            _ => return kInvalidArgument,
        };
        info.id = index as ParamID;
        copy_wstr(title, &mut info.title);
        copy_wstr(title, &mut info.shortTitle);
        copy_wstr("", &mut info.units);
        info.stepCount = 0;
        info.defaultNormalizedValue = if index == 0 { 1.0 } else { 0.0 };
        info.unitId = 0;
        info.flags = flags as i32;
        kResultOk
    }

    unsafe fn getParamStringByValue(
        &self,
        _id: ParamID,
        _v: ParamValue,
        _s: *mut String128,
    ) -> tresult {
        kNotImplemented
    }

    unsafe fn getParamValueByString(
        &self,
        _id: ParamID,
        _s: *mut TChar,
        _v: *mut ParamValue,
    ) -> tresult {
        kNotImplemented
    }

    unsafe fn normalizedParamToPlain(&self, _id: ParamID, v: ParamValue) -> ParamValue {
        v
    }

    unsafe fn plainParamToNormalized(&self, _id: ParamID, v: ParamValue) -> ParamValue {
        v
    }

    unsafe fn getParamNormalized(&self, id: ParamID) -> ParamValue {
        self.values
            .lock()
            .unwrap()
            .get(id as usize)
            .copied()
            .unwrap_or(0.0)
    }

    unsafe fn setParamNormalized(&self, id: ParamID, v: ParamValue) -> tresult {
        match self.values.lock().unwrap().get_mut(id as usize) {
            Some(slot) => {
                *slot = v;
                kResultOk
            }
            None => kInvalidArgument,
        }
    }

    unsafe fn setComponentHandler(&self, handler: *mut IComponentHandler) -> tresult {
        *self.handler.lock().unwrap() =
            unsafe { ComRef::from_raw(handler) }.map(|r| r.to_com_ptr());
        kResultOk
    }

    unsafe fn createView(&self, _name: FIDString) -> *mut IPlugView {
        std::ptr::null_mut()
    }
}

struct Factory;

impl Class for Factory {
    type Interfaces = (IPluginFactory2,);
}

impl IPluginFactoryTrait for Factory {
    unsafe fn getFactoryInfo(&self, info: *mut PFactoryInfo) -> tresult {
        let info = unsafe { &mut *info };
        copy_cstr("Kadence Tests", &mut info.vendor);
        copy_cstr("", &mut info.url);
        copy_cstr("", &mut info.email);
        info.flags = PFactoryInfo_::FactoryFlags_::kUnicode as int32;
        kResultOk
    }

    unsafe fn countClasses(&self) -> i32 {
        2
    }

    unsafe fn getClassInfo(&self, index: i32, info: *mut PClassInfo) -> tresult {
        let info = unsafe { &mut *info };
        let (cid, category) = match index {
            0 => (Processor::CID, "Audio Module Class"),
            1 => (Controller::CID, "Component Controller Class"),
            _ => return kInvalidArgument,
        };
        info.cid = cid;
        info.cardinality = PClassInfo_::ClassCardinality_::kManyInstances as int32;
        copy_cstr(category, &mut info.category);
        copy_cstr(NAME, &mut info.name);
        kResultOk
    }

    unsafe fn createInstance(
        &self,
        cid: FIDString,
        iid: FIDString,
        obj: *mut *mut c_void,
    ) -> tresult {
        let instance = match unsafe { *(cid as *const TUID) } {
            Processor::CID => ComWrapper::new(Processor {
                gain: AtomicU64::new(1.0f64.to_bits()),
                peer: Mutex::new(None),
            })
            .to_com_ptr::<FUnknown>(),
            Controller::CID => ComWrapper::new(Controller {
                values: Mutex::new([1.0, 0.0]),
                handler: Mutex::new(None),
            })
            .to_com_ptr::<FUnknown>(),
            _ => None,
        };
        match instance {
            Some(instance) => unsafe {
                let ptr = instance.as_ptr();
                ((*(*ptr).vtbl).queryInterface)(ptr, iid as *const TUID, obj)
            },
            None => kInvalidArgument,
        }
    }
}

impl IPluginFactory2Trait for Factory {
    unsafe fn getClassInfo2(&self, index: i32, info: *mut PClassInfo2) -> tresult {
        let mut base: PClassInfo = unsafe { std::mem::zeroed() };
        let result = unsafe { self.getClassInfo(index, &mut base) };
        if result != kResultOk {
            return result;
        }
        let info = unsafe { &mut *info };
        info.cid = base.cid;
        info.cardinality = base.cardinality;
        info.category = base.category;
        info.name = base.name;
        info.classFlags = 0;
        copy_cstr("Fx", &mut info.subCategories);
        copy_cstr("Kadence Tests", &mut info.vendor);
        copy_cstr("1.0.0", &mut info.version);
        copy_cstr("VST 3.8.0", &mut info.sdkVersion);
        kResultOk
    }
}

#[unsafe(no_mangle)]
extern "system" fn ModuleEntry(_handle: *mut c_void) -> bool {
    true
}

#[unsafe(no_mangle)]
extern "system" fn ModuleExit() -> bool {
    true
}

#[unsafe(no_mangle)]
extern "system" fn GetPluginFactory() -> *mut IPluginFactory {
    ComWrapper::new(Factory)
        .to_com_ptr::<IPluginFactory>()
        .map_or(std::ptr::null_mut(), |p| p.into_raw())
}
