//! A tiny CLAP plugin for the host's tests.
//!
//! It is a stereo gain with a 32-sample reported latency. Note and MIDI events are echoed to the
//! note output, and each note on also sets the read-only "Last note" parameter, which the plugin
//! reports back to the host as an output parameter event.
//!
//! Loading the state `b"DAWL"` followed by a little-endian `u32` changes the reported latency;
//! `b"DAWR"` does the same and asks the host to restart the plugin.

use clack_extensions::audio_ports::{
    AudioPortFlags, AudioPortInfo, AudioPortInfoWriter, AudioPortType, PluginAudioPorts,
    PluginAudioPortsImpl,
};
use clack_extensions::latency::{PluginLatency, PluginLatencyImpl};
use clack_extensions::note_ports::{
    NoteDialect, NoteDialects, NotePortInfo, NotePortInfoWriter, PluginNotePorts,
    PluginNotePortsImpl,
};
use clack_extensions::params::{
    ParamDisplayWriter, ParamInfo, ParamInfoFlags, ParamInfoWriter, PluginAudioProcessorParams,
    PluginMainThreadParams, PluginParams,
};
use clack_extensions::state::{PluginState, PluginStateImpl};
use clack_plugin::events::event_types::ParamValueEvent;
use clack_plugin::events::spaces::CoreEventSpace;
use clack_plugin::prelude::*;
use clack_plugin::stream::{InputStream, OutputStream};
use std::ffi::CStr;
use std::fmt::Write as _;
use std::io::{Read, Write as _};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

pub const PLUGIN_ID: &str = "org.daw.test.gain";
pub const PARAM_GAIN: u32 = 1;
pub const PARAM_LAST_NOTE: u32 = 2;
pub const LATENCY: u32 = 32;

pub struct TestPlugin;

impl Plugin for TestPlugin {
    type AudioProcessor<'a> = Processor<'a>;
    type Shared<'a> = Shared;
    type MainThread<'a> = MainThread<'a>;

    fn declare_extensions(builder: &mut PluginExtensions<Self>, _shared: Option<&Shared>) {
        builder
            .register::<PluginAudioPorts>()
            .register::<PluginNotePorts>()
            .register::<PluginLatency>()
            .register::<PluginParams>()
            .register::<PluginState>();
    }
}

impl DefaultPluginFactory for TestPlugin {
    fn get_descriptor() -> PluginDescriptor {
        use clack_plugin::plugin::features::*;
        PluginDescriptor::new(PLUGIN_ID, "DAW Test Gain")
            .with_vendor("DAW")
            .with_features([AUDIO_EFFECT, STEREO])
    }

    fn new_shared(_host: HostSharedHandle<'_>) -> Result<Shared, PluginError> {
        Ok(Shared {
            gain: AtomicF64::new(1.0),
            last_note: AtomicF64::new(0.0),
            latency: AtomicU32::new(LATENCY),
        })
    }

    fn new_main_thread<'a>(
        host: HostMainThreadHandle<'a>,
        shared: &'a Shared,
    ) -> Result<MainThread<'a>, PluginError> {
        Ok(MainThread { host, shared })
    }
}

pub struct Shared {
    gain: AtomicF64,
    last_note: AtomicF64,
    latency: AtomicU32,
}

impl Shared {
    fn handle_param(&self, event: &UnknownEvent) {
        match event.as_core_event() {
            Some(CoreEventSpace::ParamValue(e))
                if e.param_id() == Some(ClapId::new(PARAM_GAIN)) =>
            {
                self.gain.store(e.value().clamp(0.0, 2.0));
            }
            _ => {}
        }
    }
}

impl PluginShared<'_> for Shared {}

pub struct MainThread<'a> {
    host: HostMainThreadHandle<'a>,
    shared: &'a Shared,
}

impl<'a> PluginMainThread<'a, Shared> for MainThread<'a> {}

pub struct Processor<'a> {
    shared: &'a Shared,
}

impl<'a> PluginAudioProcessor<'a, Shared, MainThread<'a>> for Processor<'a> {
    fn activate(
        _host: HostAudioProcessorHandle<'a>,
        _main_thread: &MainThread<'a>,
        shared: &'a Shared,
        _audio_config: PluginAudioConfiguration,
    ) -> Result<Self, PluginError> {
        Ok(Self { shared })
    }

    fn process(
        &mut self,
        _process: Process,
        mut audio: Audio,
        events: Events,
    ) -> Result<ProcessStatus, PluginError> {
        let mut port = audio
            .port_pair(0)
            .ok_or(PluginError::Message("no audio ports"))?;
        let mut channels = port
            .channels()?
            .into_f32()
            .ok_or(PluginError::Message("expected f32 audio"))?;

        let frames = channels.frames_count() as usize;
        let mut start = 0usize;
        for batch in events.input.batch() {
            for event in batch.events() {
                self.shared.handle_param(event);
                match event.as_core_event() {
                    Some(CoreEventSpace::NoteOn(e)) => {
                        let _ = events.output.try_push(e);
                        let key = f64::from(e.as_raw().key);
                        self.shared.last_note.store(key);
                        let out = ParamValueEvent::new(
                            e.header().time(),
                            ClapId::new(PARAM_LAST_NOTE),
                            Pckn::match_all(),
                            key,
                        );
                        let _ = events.output.try_push(out);
                    }
                    Some(CoreEventSpace::NoteOff(e)) => {
                        let _ = events.output.try_push(e);
                    }
                    Some(CoreEventSpace::Midi(e)) => {
                        let _ = events.output.try_push(e);
                    }
                    _ => {}
                }
            }
            let end = batch
                .next_batch_first_sample()
                .unwrap_or(frames)
                .min(frames);
            #[allow(clippy::cast_possible_truncation)]
            let gain = self.shared.gain.load() as f32;
            for pair in channels.iter_mut() {
                let out = match pair {
                    ChannelPair::InPlace(b) => b,
                    ChannelPair::InputOutput(i, o) => {
                        o[start..end].copy_from_slice(&i[start..end]);
                        o
                    }
                    _ => continue,
                };
                for s in &mut out[start..end] {
                    *s *= gain;
                }
            }
            start = end;
        }
        // Any sidechain signal shows up as an offset on the main output, so a host that feeds
        // the sidechain is caught.
        let side: f32 = audio
            .input_port(1)
            .and_then(|p| p.channels().ok()?.into_f32())
            .map_or(0.0, |c| c.iter().flatten().map(|s| s.abs()).sum());
        if side != 0.0
            && let Some(mut main) = audio.output_port(0)
            && let Some(mut outs) = main.channels()?.into_f32()
        {
            for out in outs.iter_mut() {
                for o in out.iter_mut() {
                    *o += side;
                }
            }
        }
        // The aux output is constant 1.0, so a host that wrongly mixes it in is caught.
        if let Some(mut aux) = audio.output_port(1)
            && let Some(mut chans) = aux.channels()?.into_f32()
        {
            for chan in chans.iter_mut() {
                chan.fill(1.0);
            }
        }
        Ok(ProcessStatus::ContinueIfNotQuiet)
    }
}

impl PluginAudioPortsImpl for MainThread<'_> {
    /// A main stereo port each way, plus a stereo sidechain in and a mono aux out, neither of
    /// which is main.
    fn count(&self, _is_input: bool) -> u32 {
        2
    }

    fn get(&self, index: u32, is_input: bool, writer: &mut AudioPortInfoWriter) {
        let (name, channel_count, flags, port_type): (&[u8], _, _, _) = match (index, is_input) {
            (0, _) => (b"main", 2, AudioPortFlags::IS_MAIN, AudioPortType::STEREO),
            (1, true) => (
                b"sidechain",
                2,
                AudioPortFlags::empty(),
                AudioPortType::STEREO,
            ),
            (1, false) => (b"aux", 1, AudioPortFlags::empty(), AudioPortType::MONO),
            _ => return,
        };
        writer.set(&AudioPortInfo {
            id: ClapId::new(index),
            name,
            channel_count,
            flags,
            port_type: Some(port_type),
            in_place_pair: None,
        });
    }
}

impl PluginNotePortsImpl for MainThread<'_> {
    fn count(&self, _is_input: bool) -> u32 {
        1
    }

    fn get(&self, index: u32, _is_input: bool, writer: &mut NotePortInfoWriter) {
        if index == 0 {
            writer.set(&NotePortInfo {
                id: ClapId::new(0),
                name: b"notes",
                preferred_dialect: Some(NoteDialect::Clap),
                supported_dialects: NoteDialects::CLAP | NoteDialects::MIDI,
            });
        }
    }
}

impl PluginLatencyImpl for MainThread<'_> {
    fn get(&self) -> u32 {
        self.shared.latency.load(Ordering::Relaxed)
    }
}

impl PluginStateImpl for MainThread<'_> {
    fn save(&self, output: &mut OutputStream) -> Result<(), PluginError> {
        output.write_all(b"DAWT")?;
        output.write_all(&self.shared.gain.load().to_le_bytes())?;
        Ok(())
    }

    fn load(&self, input: &mut InputStream) -> Result<(), PluginError> {
        let mut magic = [0; 4];
        input.read_exact(&mut magic)?;
        if &magic == b"DAWL" || &magic == b"DAWR" {
            let mut latency = [0; 4];
            input.read_exact(&mut latency)?;
            self.shared
                .latency
                .store(u32::from_le_bytes(latency), Ordering::Relaxed);
            if &magic == b"DAWR" {
                self.host.request_restart();
            }
            return Ok(());
        }
        let mut buf = [0; 12];
        buf[..4].copy_from_slice(&magic);
        input.read_exact(&mut buf[4..])?;
        if &buf[..4] != b"DAWT" {
            return Err(PluginError::Message("bad state magic"));
        }
        let mut gain = [0; 8];
        gain.copy_from_slice(&buf[4..]);
        self.shared.gain.store(f64::from_le_bytes(gain));
        Ok(())
    }
}

impl PluginMainThreadParams for MainThread<'_> {
    fn count(&self) -> u32 {
        2
    }

    fn get_info(&self, param_index: u32, info: &mut ParamInfoWriter) {
        match param_index {
            0 => info.set(&ParamInfo {
                id: ClapId::new(PARAM_GAIN),
                flags: ParamInfoFlags::IS_AUTOMATABLE,
                cookie: Default::default(),
                name: b"Gain",
                module: b"",
                min_value: 0.0,
                max_value: 2.0,
                default_value: 1.0,
            }),
            1 => info.set(&ParamInfo {
                id: ClapId::new(PARAM_LAST_NOTE),
                flags: ParamInfoFlags::IS_READONLY,
                cookie: Default::default(),
                name: b"Last note",
                module: b"",
                min_value: 0.0,
                max_value: 127.0,
                default_value: 0.0,
            }),
            _ => {}
        }
    }

    fn get_value(&self, param_id: ClapId) -> Option<f64> {
        match param_id.get() {
            PARAM_GAIN => Some(self.shared.gain.load()),
            PARAM_LAST_NOTE => Some(self.shared.last_note.load()),
            _ => None,
        }
    }

    fn value_to_text(
        &self,
        _param_id: ClapId,
        value: f64,
        writer: &mut ParamDisplayWriter,
    ) -> std::fmt::Result {
        write!(writer, "{value:.2}")
    }

    fn text_to_value(&self, _param_id: ClapId, text: &CStr) -> Option<f64> {
        text.to_str().ok()?.trim().parse().ok()
    }

    fn flush(&self, input: &InputEvents, _output: &mut OutputEvents) {
        for event in input {
            self.shared.handle_param(event);
        }
    }
}

impl PluginAudioProcessorParams for Processor<'_> {
    fn flush(&mut self, input: &InputEvents, _output: &mut OutputEvents) {
        for event in input {
            self.shared.handle_param(event);
        }
    }
}

struct AtomicF64(AtomicU64);

impl AtomicF64 {
    fn new(value: f64) -> Self {
        Self(AtomicU64::new(value.to_bits()))
    }

    fn store(&self, value: f64) {
        self.0.store(value.to_bits(), Ordering::Relaxed)
    }

    fn load(&self) -> f64 {
        f64::from_bits(self.0.load(Ordering::Relaxed))
    }
}

clack_export_entry!(SinglePluginEntry<TestPlugin>);
