//! Track outputs, sends and buses, played through a session.

use std::path::Path;

use daw_core::event::EventList;
use daw_core::node::{Node, PortLayout, ProcessContext};
use daw_core::plugin::{
    ParamInfo, PluginDescriptor, PluginError, PluginFormatKind, PluginInstance,
};
use daw_core::time::{PPQ, SampleTime, TickTime};
use daw_engine::{EngineConfig, EngineProcessor};
use daw_media::{AudioData, WavFormat, write_wav};
use daw_model::commands::{
    AddClip, AddDevice, AddSend, AddTrack, SendField, SetSend, SetTrack, TrackField,
};
use daw_model::{
    AudioClip, AudioInput, Clip, ClipContent, Device, DeviceSource, Document, InputChannels,
    Monitoring, Project, Send, SendId, Track, TrackId, TrackKind, TrackOutput,
};
use daw_session::{LoadError, LoadedPlugin, PluginLoader, Session, SessionConfig, Warning};

const RATE: f64 = 48_000.0;
const BLOCK: usize = 256;
/// Samples per beat at the default 120 bpm.
const BEAT: usize = 24_000;

struct Rig {
    doc: Document,
    session: Session,
    proc: EngineProcessor,
    track: TrackId,
    _dir: tempfile::TempDir,
}

impl Rig {
    /// An audio track playing a beat of DC at 0.5 from the start.
    fn new() -> Self {
        Self::with(0, &[0.5])
    }

    /// An audio track playing a beat of DC, at one level per channel of its file, in a
    /// session with `inputs` hardware inputs and the [`Fake`] plugins.
    fn with(inputs: u16, levels: &[f32]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        write_dc(dir.path(), "a.wav", levels);
        let mut doc = Document::new(Project::new("routing"));
        let track = Track::new(TrackKind::Audio, "Audio 1");
        let id = track.id;
        doc.execute(AddTrack::new(0, track)).unwrap();
        let clip = Clip::new(
            "clip",
            TickTime(0),
            TickTime(PPQ),
            ClipContent::Audio(AudioClip::new("a.wav", SampleTime(0))),
        );
        doc.execute(AddClip::new(id, clip)).unwrap();
        let (mut session, proc) = Session::new(SessionConfig {
            engine: EngineConfig {
                sample_rate: RATE,
                max_frames: BLOCK as u32,
                ..Default::default()
            },
            inputs,
            outputs: 2,
        });
        session.set_plugin_loader(Some(Box::new(FakeLoader)));
        session.set_root(Some(dir.path().to_path_buf()));
        let mut rig = Self {
            doc,
            session,
            proc,
            track: id,
            _dir: dir,
        };
        rig.sync();
        rig
    }

    fn sync(&mut self) {
        let snap = self.doc.snapshot();
        self.session.sync(&snap).unwrap();
    }

    fn bus(&mut self, name: &str) -> TrackId {
        let bus = Track::new(TrackKind::Bus, name);
        let id = bus.id;
        let at = self.doc.project().tracks.len();
        self.doc.execute(AddTrack::new(at, bus)).unwrap();
        id
    }

    fn send(&mut self, from: TrackId, to: TrackId) -> SendId {
        let send = Send::new(to);
        let id = send.id;
        self.doc.execute(AddSend { track: from, send }).unwrap();
        id
    }

    fn set(&mut self, track: TrackId, field: TrackField) {
        self.doc.execute(SetTrack::new(track, field)).unwrap();
    }

    fn plugin(&mut self, track: TrackId, index: usize, id: &str) {
        let source = DeviceSource::Plugin {
            format: PluginFormatKind::Clap,
            id: id.into(),
        };
        let device = Device::new(id, source);
        self.doc
            .execute(AddDevice {
                track,
                index,
                device,
            })
            .unwrap();
    }

    fn pre_fader_send(&mut self, from: TrackId, to: TrackId) -> SendId {
        let id = self.send(from, to);
        let field = SendField::PreFader(true);
        self.doc.execute(SetSend { id, field }).unwrap();
        id
    }

    /// Plays `frames` from the start and returns the left channel.
    fn play(&mut self, frames: usize) -> Vec<f32> {
        let [left, _] = self.play_stereo(frames);
        left
    }

    /// Plays `frames` from the start and returns both channels.
    fn play_stereo(&mut self, frames: usize) -> [Vec<f32>; 2] {
        self.sync();
        self.session.stop().unwrap();
        self.session.locate(TickTime(0)).unwrap();
        self.session.play().unwrap();
        let out = self.process(frames, 0.0);
        self.session.collect_garbage();
        out
    }

    /// Runs `frames` with every hardware input at `input` and returns both channels.
    fn process(&mut self, frames: usize, input: f32) -> [Vec<f32>; 2] {
        let mut out = [Vec::with_capacity(frames), Vec::with_capacity(frames)];
        let mut l = vec![0.0; BLOCK];
        let mut r = vec![0.0; BLOCK];
        let events = EventList::with_capacity(0);
        let inputs = vec![vec![input; BLOCK]; self.session.config().inputs as usize];
        while out[0].len() < frames {
            let n = BLOCK.min(frames - out[0].len());
            let ins: Vec<&[f32]> = inputs.iter().map(|c| &c[..n]).collect();
            let mut outs: [&mut [f32]; 2] = [&mut l[..n], &mut r[..n]];
            self.proc.process(n, &ins, &mut outs, &events);
            out[0].extend_from_slice(&l[..n]);
            out[1].extend_from_slice(&r[..n]);
        }
        out
    }
}

/// What a [`Fake`] with no inputs plays.
const FAKE_LEVEL: f32 = 0.25;

/// A plugin with the channel counts its id names ("1x1": one input, one output) and no
/// event input. It passes its first input on to every output, or with no inputs plays
/// [`FAKE_LEVEL`].
struct Fake {
    descriptor: PluginDescriptor,
}

impl Node for Fake {
    fn ports(&self) -> PortLayout {
        PortLayout {
            audio_in: self.descriptor.ports.0,
            audio_out: self.descriptor.ports.1,
            ..Default::default()
        }
    }
    fn prepare(&mut self, _sample_rate: f64, _max_frames: u32) {}
    fn process(&mut self, ctx: &mut ProcessContext) {
        for out in ctx.audio_out.iter_mut() {
            match ctx.audio_in.first() {
                Some(input) => out.copy_from_slice(input),
                None => out.fill(FAKE_LEVEL),
            }
        }
    }
}

impl PluginInstance for Fake {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }
    fn params(&self) -> Vec<ParamInfo> {
        Vec::new()
    }
    fn param_value(&self, _id: u32) -> f64 {
        0.0
    }
    fn save_state(&mut self) -> Result<Vec<u8>, PluginError> {
        Ok(Vec::new())
    }
    fn load_state(&mut self, _data: &[u8]) -> Result<(), PluginError> {
        Ok(())
    }
    fn has_editor(&self) -> bool {
        false
    }
    fn open_editor(&mut self) -> Result<(), PluginError> {
        Err(PluginError::Editor("none".into()))
    }
    fn close_editor(&mut self) {}
}

struct FakeLoader;

impl PluginLoader for FakeLoader {
    fn load(
        &mut self,
        format: &PluginFormatKind,
        id: &str,
        _sample_rate: f64,
        _max_frames: u32,
    ) -> Result<LoadedPlugin, LoadError> {
        let ports = id.split_once('x').ok_or(LoadError::NotFound)?;
        let count = |s: &str| s.parse::<u16>().map_err(|_| LoadError::NotFound);
        let descriptor = PluginDescriptor {
            format: format.clone(),
            id: id.into(),
            name: id.into(),
            vendor: String::new(),
            path: Default::default(),
            is_instrument: false,
            ports: (count(ports.0)?, count(ports.1)?),
        };
        Ok(LoadedPlugin {
            instance: Box::new(Fake { descriptor }),
            monitor: None,
        })
    }
}

fn write_dc(dir: &Path, name: &str, levels: &[f32]) {
    let data = AudioData {
        sample_rate: RATE as u32,
        channels: levels.iter().map(|&level| vec![level; BEAT]).collect(),
    };
    write_wav(dir.join(name), &data, WavFormat::Float32).unwrap();
}

fn peak(s: &[f32]) -> f32 {
    s.iter().fold(0.0, |m, x| m.max(x.abs()))
}

/// The middle of the clip, past its edge fades.
fn during(out: &[f32]) -> f32 {
    out[BEAT / 2]
}

#[test]
fn a_send_feeds_a_reverb_bus() {
    let mut rig = Rig::new();
    let bus = rig.bus("Reverb");
    let reverb = Device::new(
        "Reverb",
        DeviceSource::Builtin {
            kind: "daw.reverb".into(),
        },
    );
    rig.doc
        .execute(AddDevice {
            track: bus,
            index: 0,
            device: reverb,
        })
        .unwrap();
    // The dry clip stops after a beat; without a send nothing follows it.
    let dry = rig.play(2 * BEAT);
    let tail = BEAT + 2_000..BEAT + 12_000;
    assert_eq!(peak(&dry[tail.clone()]), 0.0, "no tail without a send");

    rig.send(rig.track, bus);
    let wet = rig.play(2 * BEAT);
    assert!(peak(&wet[tail.clone()]) > 1e-3, "the reverb rings on");

    // Undoing the send takes the reverb away again: once the old tail has
    // died down, playing the clip leaves it silent.
    rig.doc.undo().unwrap();
    rig.play(40 * BEAT);
    let undone = rig.play(2 * BEAT);
    assert!(peak(&undone[tail]) < 1e-5);
}

#[test]
fn a_pre_fader_send_with_nothing_to_tap_still_syncs() {
    // An instrument track without devices has no signal before its fader.
    let mut rig = Rig::new();
    let bus = rig.bus("Aux");
    let keys = Track::new(TrackKind::Instrument, "Keys");
    let id = keys.id;
    rig.doc.execute(AddTrack::new(0, keys)).unwrap();
    let send = rig.send(id, bus);
    rig.doc
        .execute(SetSend {
            id: send,
            field: SendField::PreFader(true),
        })
        .unwrap();
    rig.sync();
    rig.doc
        .execute(SetSend {
            id: send,
            field: SendField::LevelDb(-6.0),
        })
        .unwrap();
    rig.sync();
}

#[test]
fn a_track_output_goes_through_its_bus() {
    let mut rig = Rig::new();
    let bus = rig.bus("Group");
    rig.set(bus, TrackField::VolumeDb(-6.0));
    let direct = during(&rig.play(BEAT));
    assert!((direct - 0.5).abs() < 1e-6, "{direct}");

    rig.set(rig.track, TrackField::Output(TrackOutput::Bus(bus)));
    let grouped = during(&rig.play(BEAT));
    let half = 0.5 * 10f32.powf(-6.0 / 20.0);
    assert!(
        (grouped - half).abs() < 1e-4,
        "{grouped} through the bus fader"
    );

    rig.doc.undo().unwrap();
    let back = during(&rig.play(BEAT));
    assert!(
        (back - 0.5).abs() < 1e-6,
        "{back} straight to the master again"
    );
}

#[test]
fn pre_fader_sends_ignore_the_fader() {
    let mut rig = Rig::new();
    let bus = rig.bus("Monitor");
    let send = rig.send(rig.track, bus);
    rig.set(rig.track, TrackField::VolumeDb(-144.0));
    assert_eq!(during(&rig.play(BEAT)), 0.0, "post-fader follows the fader");

    rig.doc
        .execute(SetSend {
            id: send,
            field: SendField::PreFader(true),
        })
        .unwrap();
    let pre = during(&rig.play(BEAT));
    assert!((pre - 0.5).abs() < 1e-6, "{pre}");
}

#[test]
fn a_bus_without_devices_has_a_pre_fader_signal() {
    let mut rig = Rig::new();
    let group = rig.bus("Group");
    let aux = rig.bus("Aux");
    rig.set(rig.track, TrackField::Output(TrackOutput::Bus(group)));
    rig.set(group, TrackField::VolumeDb(-144.0));
    let send = rig.send(group, aux);
    rig.doc
        .execute(SetSend {
            id: send,
            field: SendField::PreFader(true),
        })
        .unwrap();
    let out = during(&rig.play(BEAT));
    assert!(
        (out - 0.5).abs() < 1e-6,
        "{out} from the group before its fader"
    );

    // Back to post-fader: the silent group fader silences the send.
    rig.doc.undo().unwrap();
    assert_eq!(during(&rig.play(BEAT)), 0.0);
}

#[test]
fn a_routing_loop_is_left_out_and_reported() {
    // Commands refuse loops, so build one by hand, as a damaged file could.
    let mut project = Project::new("loop");
    let mut a = Track::new(TrackKind::Bus, "A");
    let mut b = Track::new(TrackKind::Bus, "B");
    a.output = TrackOutput::Bus(b.id);
    b.sends.push(Send::new(a.id));
    let track = Track::new(TrackKind::Audio, "Audio");
    project.tracks = vec![track.into(), a.into(), b.into()];
    assert!(project.validate().is_err());
    let mut doc = Document::new(project);
    let (mut session, _proc) = Session::new(SessionConfig {
        inputs: 0,
        ..Default::default()
    });
    session
        .sync(&doc.snapshot())
        .expect("plays without the loop");
    assert_eq!(
        session.warnings(),
        [Warning::RoutingLoop { track: "B".into() }]
    );
}

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() < 1e-6
}

/// Both channels in the middle of the clip.
fn during_stereo(out: &[Vec<f32>; 2]) -> (f32, f32) {
    (during(&out[0]), during(&out[1]))
}

#[test]
fn a_mono_effect_gets_both_channels_and_plays_on_both() {
    let mut rig = Rig::with(0, &[0.5, 0.1]);
    let (l, r) = during_stereo(&rig.play_stereo(BEAT));
    assert!(close(l, 0.5) && close(r, 0.1), "{l} {r} without the effect");

    rig.plugin(rig.track, 0, "1x1");
    let (l, r) = during_stereo(&rig.play_stereo(BEAT));
    assert!(close(l, 0.3) && close(r, 0.3), "{l} {r}: the mix of both");

    // A mono effect first on a bus: what is routed into the bus goes the same way.
    rig.doc.undo().unwrap();
    let bus = rig.bus("Group");
    rig.set(rig.track, TrackField::Output(TrackOutput::Bus(bus)));
    rig.plugin(bus, 0, "1x1");
    let (l, r) = during_stereo(&rig.play_stereo(BEAT));
    assert!(close(l, 0.3) && close(r, 0.3), "{l} {r} through the bus");

    // Into a stereo effect after it, the mono output goes to both inputs.
    rig.plugin(bus, 1, "2x2");
    let (l, r) = during_stereo(&rig.play_stereo(BEAT));
    assert!(
        close(l, 0.3) && close(r, 0.3),
        "{l} {r} through both effects"
    );
}

#[test]
fn a_mono_instrument_plays_on_both_channels() {
    let mut rig = Rig::new();
    let keys = Track::new(TrackKind::Instrument, "Keys");
    let id = keys.id;
    rig.doc.execute(AddTrack::new(1, keys)).unwrap();
    rig.set(rig.track, TrackField::Mute(true));
    rig.plugin(id, 0, "0x1");
    let (l, r) = during_stereo(&rig.play_stereo(BEAT));
    assert!(close(l, FAKE_LEVEL) && close(r, FAKE_LEVEL), "{l} {r}");
}

/// A device without audio inputs on a bus (an instrument, a tone generator) adds its sound
/// to what is routed into the bus instead of replacing it.
#[test]
fn a_device_without_inputs_adds_to_the_signal() {
    let mut rig = Rig::new();
    let bus = rig.bus("Group");
    rig.set(rig.track, TrackField::Output(TrackOutput::Bus(bus)));
    rig.plugin(bus, 0, "0x2");
    let out = during(&rig.play(BEAT));
    assert!(close(out, 0.5 + FAKE_LEVEL), "{out}");
    // After an effect, too: the effect's output and the generator's are mixed.
    rig.plugin(bus, 0, "2x2");
    let out = during(&rig.play(BEAT));
    assert!(close(out, 0.5 + FAKE_LEVEL), "{out}");
}

/// The engine refuses a graph with events sent to a node that takes none, and with it every
/// later edit. Notes skip such a device.
#[test]
fn an_instrument_track_syncs_with_a_first_device_that_takes_no_events() {
    let mut rig = Rig::new();
    let keys = Track::new(TrackKind::Instrument, "Keys");
    let id = keys.id;
    rig.doc.execute(AddTrack::new(1, keys)).unwrap();
    rig.plugin(id, 0, "2x2");
    rig.sync();
    assert!(rig.session.warnings().is_empty());
    // An edit after it still reaches the engine.
    rig.set(rig.track, TrackField::VolumeDb(-144.0));
    assert_eq!(during(&rig.play(BEAT)), 0.0);
}

#[test]
fn mute_and_solo_silence_pre_fader_sends() {
    let mut rig = Rig::new();
    let bus = rig.bus("Reverb");
    rig.pre_fader_send(rig.track, bus);
    // Only the send is heard.
    rig.set(rig.track, TrackField::VolumeDb(-144.0));
    assert!(close(during(&rig.play(BEAT)), 0.5));

    rig.set(rig.track, TrackField::Mute(true));
    assert_eq!(during(&rig.play(BEAT)), 0.0, "muted");
    rig.set(rig.track, TrackField::Mute(false));
    assert!(close(during(&rig.play(BEAT)), 0.5), "and back");

    // Another track soloed, which feeds the same bus: the bus stays audible, the first
    // track's send must not.
    let other = Track::new(TrackKind::Audio, "Audio 2");
    let other_id = other.id;
    rig.doc.execute(AddTrack::new(1, other)).unwrap();
    rig.send(other_id, bus);
    rig.set(other_id, TrackField::Solo(true));
    assert_eq!(during(&rig.play(BEAT)), 0.0, "not part of the solo");
    rig.set(other_id, TrackField::Solo(false));
    assert!(close(during(&rig.play(BEAT)), 0.5));

    // Switching the send back to post-fader takes the stage out again.
    rig.doc.undo().unwrap();
    rig.sync();
}

/// The usual cue mix while tracking: an armed track without effects, monitored, with a
/// pre-fader send. The send carries the input, not only the track's clips.
#[test]
fn a_pre_fader_send_carries_the_monitored_input() {
    let mut rig = Rig::with(2, &[0.5]);
    let cue = rig.bus("Cue");
    rig.pre_fader_send(rig.track, cue);
    rig.set(rig.track, TrackField::VolumeDb(-144.0));
    rig.set(rig.track, TrackField::Armed(true));
    rig.set(
        rig.track,
        TrackField::AudioInput(AudioInput {
            channels: InputChannels::Stereo(0),
            monitor: Monitoring::Auto,
        }),
    );
    rig.sync();
    assert!(rig.session.is_monitoring(rig.track));
    // Stopped, so no clip plays: what comes out is the input, through the send.
    let [left, right] = rig.process(8 * BLOCK, 0.2);
    let (l, r) = (left[left.len() - 1], right[right.len() - 1]);
    assert!(close(l, 0.2) && close(r, 0.2), "{l} {r}");
}
