//! Track outputs, sends and buses, played through a session.

use std::path::Path;

use daw_core::event::EventList;
use daw_core::time::{PPQ, SampleTime, TickTime};
use daw_engine::{EngineConfig, EngineProcessor};
use daw_media::{AudioData, WavFormat, write_wav};
use daw_model::commands::{
    AddClip, AddDevice, AddSend, AddTrack, SendField, SetSend, SetTrack, TrackField,
};
use daw_model::{
    AudioClip, Clip, ClipContent, Device, DeviceSource, Document, Project, Send, SendId, Track,
    TrackId, TrackKind, TrackOutput,
};
use daw_session::{Session, SessionConfig, Warning};

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
        let dir = tempfile::tempdir().unwrap();
        write_dc(dir.path(), "a.wav");
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
            inputs: 0,
            outputs: 2,
        });
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

    /// Plays `frames` from the start and returns the left channel.
    fn play(&mut self, frames: usize) -> Vec<f32> {
        self.sync();
        self.session.stop().unwrap();
        self.session.locate(TickTime(0)).unwrap();
        self.session.play().unwrap();
        let mut out = Vec::with_capacity(frames);
        let mut l = vec![0.0; BLOCK];
        let mut r = vec![0.0; BLOCK];
        let events = EventList::with_capacity(0);
        while out.len() < frames {
            let n = BLOCK.min(frames - out.len());
            let mut outs: [&mut [f32]; 2] = [&mut l[..n], &mut r[..n]];
            self.proc.process(n, &[], &mut outs, &events);
            out.extend_from_slice(&l[..n]);
        }
        self.session.collect_garbage();
        out
    }
}

fn write_dc(dir: &Path, name: &str) {
    let data = AudioData {
        sample_rate: RATE as u32,
        channels: vec![vec![0.5; BEAT]],
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
