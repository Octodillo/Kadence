//! Property tests: random edit sequences undo to the exact starting model,
//! redo to the exact final model, keep the model valid, and survive a
//! save/load round trip unchanged.

use daw_core::plugin::PluginFormatKind;
use daw_core::time::{SampleTime, TickTime};
use daw_model::commands::*;
use daw_model::*;
use proptest::prelude::*;

#[derive(Clone, Debug)]
enum Op {
    AddTrack(u8, usize),
    RemoveTrack(usize),
    MoveTrack(usize, usize),
    Volume(usize, f64),
    Pan(usize, f64),
    Rename(usize, String),
    Mute(usize, bool),
    Output(usize, usize),
    AddClip(usize, i64, i64),
    RemoveClip(usize),
    MoveClip(usize, usize, i64),
    ClipLength(usize, i64),
    ClipGain(usize, f64),
    AddNote(usize, i64, i64, u8, f64),
    RemoveNote(usize),
    EditNote(usize, i64, u8, f64),
    AddDevice(usize, usize, bool),
    RemoveDevice(usize),
    MoveDevice(usize, usize),
    Param(usize, u32, Option<f64>),
    DeviceState(usize, Option<Vec<u8>>),
    AddSend(usize, usize),
    RemoveSend(usize),
    SendLevel(usize, f64),
    SendTarget(usize, usize),
    AddLane(usize, u8),
    RemoveLane(usize),
    LanePoints(usize, Vec<(i64, f64)>),
    Tempo(f64),
    Batch(Vec<(usize, f64)>),
    Undo,
    Seal,
}

fn value() -> impl Strategy<Value = f64> {
    prop_oneof![
        8 => -60.0..12.0f64,
        1 => Just(f64::NAN),
        1 => Just(f64::INFINITY),
    ]
}

fn op() -> impl Strategy<Value = Op> {
    let i = || 0usize..64;
    prop_oneof![
        (0u8..4, i()).prop_map(|(k, x)| Op::AddTrack(k, x)),
        i().prop_map(Op::RemoveTrack),
        (i(), i()).prop_map(|(a, b)| Op::MoveTrack(a, b)),
        (i(), value()).prop_map(|(a, v)| Op::Volume(a, v)),
        (i(), -1.5..1.5f64).prop_map(|(a, v)| Op::Pan(a, v)),
        (i(), "[a-z ]{0,8}").prop_map(|(a, s)| Op::Rename(a, s)),
        (i(), any::<bool>()).prop_map(|(a, b)| Op::Mute(a, b)),
        (i(), i()).prop_map(|(a, b)| Op::Output(a, b)),
        (i(), -10i64..20_000, -10i64..5_000).prop_map(|(a, s, l)| Op::AddClip(a, s, l)),
        i().prop_map(Op::RemoveClip),
        (i(), i(), -10i64..20_000).prop_map(|(a, b, s)| Op::MoveClip(a, b, s)),
        (i(), -10i64..5_000).prop_map(|(a, l)| Op::ClipLength(a, l)),
        (i(), value()).prop_map(|(a, v)| Op::ClipGain(a, v)),
        (i(), 0i64..4_000, -10i64..1_000, 0u8..140, -0.2..1.2f64)
            .prop_map(|(a, s, l, k, v)| Op::AddNote(a, s, l, k, v)),
        i().prop_map(Op::RemoveNote),
        (i(), 0i64..4_000, 0u8..128, 0.0..1.0f64).prop_map(|(a, s, k, v)| Op::EditNote(a, s, k, v)),
        (i(), 0usize..4, any::<bool>()).prop_map(|(a, x, p)| Op::AddDevice(a, x, p)),
        i().prop_map(Op::RemoveDevice),
        (i(), 0usize..4).prop_map(|(a, b)| Op::MoveDevice(a, b)),
        (i(), 0u32..4, proptest::option::of(value())).prop_map(|(a, p, v)| Op::Param(a, p, v)),
        (
            i(),
            proptest::option::of(proptest::collection::vec(any::<u8>(), 0..32))
        )
            .prop_map(|(a, s)| Op::DeviceState(a, s)),
        (i(), i()).prop_map(|(a, b)| Op::AddSend(a, b)),
        i().prop_map(Op::RemoveSend),
        (i(), value()).prop_map(|(a, v)| Op::SendLevel(a, v)),
        (i(), i()).prop_map(|(a, b)| Op::SendTarget(a, b)),
        (i(), 0u8..4).prop_map(|(a, k)| Op::AddLane(a, k)),
        i().prop_map(Op::RemoveLane),
        (
            i(),
            proptest::collection::vec((0i64..10_000, -1.0..1.0f64), 0..6)
        )
            .prop_map(|(a, p)| Op::LanePoints(a, p)),
        prop_oneof![20.0..300.0f64, Just(0.0), Just(-5.0)].prop_map(Op::Tempo),
        proptest::collection::vec((i(), value()), 1..4).prop_map(Op::Batch),
        Just(Op::Undo),
        Just(Op::Seal),
    ]
}

fn pick<T: Copy>(items: &[T], i: usize) -> Option<T> {
    (!items.is_empty()).then(|| items[i % items.len()])
}

fn track_ids(p: &Project) -> Vec<TrackId> {
    p.all_tracks().map(|t| t.id).collect()
}

fn clip_ids(p: &Project) -> Vec<ClipId> {
    p.all_tracks()
        .flat_map(|t| t.clips.iter().map(|c| c.id))
        .collect()
}

fn midi_notes(p: &Project) -> Vec<(ClipId, NoteUid)> {
    p.all_tracks()
        .flat_map(|t| t.clips.iter())
        .flat_map(|c| match &c.content {
            ClipContent::Midi(m) => m.notes.iter().map(|n| (c.id, n.id)).collect(),
            ClipContent::Audio(_) => Vec::new(),
        })
        .collect()
}

fn device_ids(p: &Project) -> Vec<DeviceId> {
    p.all_tracks()
        .flat_map(|t| t.devices.iter().map(|d| d.id))
        .collect()
}

fn send_ids(p: &Project) -> Vec<SendId> {
    p.all_tracks()
        .flat_map(|t| t.sends.iter().map(|s| s.id))
        .collect()
}

fn lane_ids(p: &Project) -> Vec<LaneId> {
    p.all_tracks()
        .flat_map(|t| t.automation.iter().map(|l| l.id))
        .collect()
}

/// Turns an abstract op into a concrete command against the current model.
/// Returns `None` when the op has nothing to act on.
fn build(p: &Project, op: &Op) -> Option<Box<dyn Command>> {
    let tracks = track_ids(p);
    let cmd: Box<dyn Command> = match op.clone() {
        Op::AddTrack(k, i) => {
            let kind = [
                TrackKind::Audio,
                TrackKind::Instrument,
                TrackKind::Bus,
                TrackKind::Master,
            ][k as usize];
            Box::new(AddTrack::new(
                i % (p.tracks.len() + 2),
                Track::new(kind, "New"),
            ))
        }
        Op::RemoveTrack(i) => Box::new(RemoveTrack::new(pick(&tracks, i)?)),
        Op::MoveTrack(i, to) => {
            Box::new(MoveTrack::new(pick(&tracks, i)?, to % (p.tracks.len() + 1)))
        }
        Op::Volume(i, v) => Box::new(SetTrack::new(pick(&tracks, i)?, TrackField::VolumeDb(v))),
        Op::Pan(i, v) => Box::new(SetTrack::new(pick(&tracks, i)?, TrackField::Pan(v))),
        Op::Rename(i, s) => Box::new(SetTrack::new(pick(&tracks, i)?, TrackField::Name(s))),
        Op::Mute(i, b) => Box::new(SetTrack::new(pick(&tracks, i)?, TrackField::Mute(b))),
        Op::Output(i, j) => {
            let out = if j % 3 == 0 {
                TrackOutput::Master
            } else {
                TrackOutput::Bus(pick(&tracks, j)?)
            };
            Box::new(SetTrack::new(pick(&tracks, i)?, TrackField::Output(out)))
        }
        Op::AddClip(i, s, l) => {
            let t = pick(&tracks, i)?;
            let content = if p.track(t)?.kind == TrackKind::Instrument {
                ClipContent::Midi(MidiClip::default())
            } else {
                ClipContent::Audio(AudioClip {
                    source: "audio/a.wav".into(),
                    offset: SampleTime(0),
                    gain_db: 0.0,
                })
            };
            Box::new(AddClip::new(
                t,
                Clip::new("Clip", TickTime(s), TickTime(l), content),
            ))
        }
        Op::RemoveClip(i) => Box::new(RemoveClip::new(pick(&clip_ids(p), i)?)),
        Op::MoveClip(i, j, s) => Box::new(MoveClip::new(
            pick(&clip_ids(p), i)?,
            pick(&tracks, j)?,
            TickTime(s),
        )),
        Op::ClipLength(i, l) => Box::new(SetClip::new(
            pick(&clip_ids(p), i)?,
            ClipField::Length(TickTime(l)),
        )),
        Op::ClipGain(i, v) => Box::new(SetClip::new(
            pick(&clip_ids(p), i)?,
            ClipField::AudioGainDb(v),
        )),
        Op::AddNote(i, s, l, k, v) => Box::new(AddNotes {
            clip: pick(&clip_ids(p), i)?,
            notes: vec![Note::new(TickTime(s), TickTime(l), k, v)],
        }),
        Op::RemoveNote(i) => {
            let (clip, note) = pick(&midi_notes(p), i)?;
            Box::new(RemoveNotes::new(clip, vec![note]))
        }
        Op::EditNote(i, s, k, v) => {
            let (clip, id) = pick(&midi_notes(p), i)?;
            let mut note = Note::new(TickTime(s), TickTime(120), k, v);
            note.id = id;
            Box::new(EditNotes {
                clip,
                notes: vec![note],
            })
        }
        Op::AddDevice(i, index, plugin) => {
            let source = if plugin {
                DeviceSource::Plugin {
                    format: PluginFormatKind::Clap,
                    id: "org.example.synth".into(),
                }
            } else {
                DeviceSource::Builtin {
                    kind: "gain".into(),
                }
            };
            Box::new(AddDevice {
                track: pick(&tracks, i)?,
                index,
                device: Device::new("Dev", source),
            })
        }
        Op::RemoveDevice(i) => Box::new(RemoveDevice::new(pick(&device_ids(p), i)?)),
        Op::MoveDevice(i, to) => Box::new(MoveDevice::new(pick(&device_ids(p), i)?, to)),
        Op::Param(i, param, value) => Box::new(SetParam {
            device: pick(&device_ids(p), i)?,
            param,
            value,
        }),
        Op::DeviceState(i, s) => Box::new(SetDevice {
            id: pick(&device_ids(p), i)?,
            field: DeviceField::State(s.map(StateBlob::from)),
        }),
        Op::AddSend(i, j) => Box::new(AddSend {
            track: pick(&tracks, i)?,
            send: Send::new(pick(&tracks, j)?),
        }),
        Op::RemoveSend(i) => Box::new(RemoveSend::new(pick(&send_ids(p), i)?)),
        Op::SendLevel(i, v) => Box::new(SetSend {
            id: pick(&send_ids(p), i)?,
            field: SendField::LevelDb(v),
        }),
        Op::SendTarget(i, j) => Box::new(SetSend {
            id: pick(&send_ids(p), i)?,
            field: SendField::Target(pick(&tracks, j)?),
        }),
        Op::AddLane(i, k) => {
            let t = p.track(pick(&tracks, i)?)?;
            let target = match k {
                0 => AutomationTarget::Volume,
                1 => AutomationTarget::Pan,
                2 => AutomationTarget::SendLevel {
                    send: t.sends.first()?.id,
                },
                _ => AutomationTarget::DeviceParam {
                    device: t.devices.first()?.id,
                    param: 0,
                },
            };
            Box::new(AddLane {
                track: t.id,
                lane: AutomationLane::new(target),
            })
        }
        Op::RemoveLane(i) => Box::new(RemoveLane::new(pick(&lane_ids(p), i)?)),
        Op::LanePoints(i, pts) => Box::new(SetLane {
            id: pick(&lane_ids(p), i)?,
            field: LaneField::Points(
                pts.into_iter()
                    .map(|(t, v)| AutomationPoint {
                        tick: TickTime(t),
                        value: v,
                        curve: 0.0,
                        step: false,
                    })
                    .collect(),
            ),
        }),
        Op::Tempo(bpm) => {
            let mut tempo = p.tempo.clone();
            tempo.tempos[0].bpm = bpm;
            Box::new(SetTempo { tempo })
        }
        Op::Batch(edits) => Box::new(Batch::new(
            "Batch",
            edits
                .into_iter()
                .filter_map(|(i, v)| {
                    pick(&tracks, i).map(|t| {
                        Box::new(SetTrack::new(t, TrackField::VolumeDb(v))) as Box<dyn Command>
                    })
                })
                .collect(),
        )),
        Op::Undo | Op::Seal => return None,
    };
    Some(cmd)
}

/// A small project with every kind of object in it.
fn seed() -> Project {
    let mut doc = Document::default();
    let bus = Track::new(TrackKind::Bus, "Bus");
    let bus_id = bus.id;
    let audio = Track::new(TrackKind::Audio, "Audio");
    let audio_id = audio.id;
    let mut inst = Track::new(TrackKind::Instrument, "Synth");
    inst.output = TrackOutput::Bus(bus_id);
    let mut clip = MidiClip::default();
    clip.notes
        .push(Note::new(TickTime(0), TickTime(480), 60, 0.8));
    inst.clips
        .push(Clip::new("Riff", TickTime(0), TickTime(3840), ClipContent::Midi(clip)).into());
    doc.execute(AddTrack::new(0, bus)).unwrap();
    doc.execute(AddTrack::new(0, audio)).unwrap();
    doc.execute(AddTrack::new(1, inst)).unwrap();
    doc.execute(AddSend {
        track: audio_id,
        send: Send::new(bus_id),
    })
    .unwrap();
    doc.execute(AddDevice {
        track: audio_id,
        index: 0,
        device: Device::new("EQ", DeviceSource::Builtin { kind: "eq".into() }),
    })
    .unwrap();
    doc.project().clone()
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    #[test]
    fn undo_all_restores_redo_all_replays(ops in proptest::collection::vec(op(), 1..60)) {
        let start = seed();
        let mut doc = Document::new(start.clone());
        for op in &ops {
            match op {
                Op::Undo => { doc.undo().unwrap(); }
                Op::Seal => doc.seal(),
                _ => {
                    let Some(cmd) = build(doc.project(), op) else { continue };
                    let before = doc.project().clone();
                    if doc.execute_boxed(cmd).is_err() {
                        prop_assert_eq!(doc.project(), &before, "failed command changed the model");
                    }
                }
            }
            prop_assert_eq!(doc.project().validate(), Ok(()));
        }
        let end = doc.project().clone();
        let steps = doc.history().undo_len();

        // Lossless save/load of whatever we ended up with.
        let json = bundle::to_json(&end).unwrap();
        prop_assert_eq!(&bundle::from_json(&json).unwrap(), &end);

        while doc.undo().unwrap().is_some() {}
        prop_assert_eq!(doc.project(), &start);
        prop_assert!(!doc.is_modified());

        for _ in 0..steps {
            prop_assert!(doc.redo().unwrap().is_some());
        }
        prop_assert_eq!(doc.project(), &end);
    }

    #[test]
    fn merged_edits_undo_in_one_step(values in proptest::collection::vec(-60.0..12.0f64, 2..20)) {
        let start = seed();
        let id = start.tracks[0].id;
        let mut doc = Document::new(start.clone());
        for v in &values {
            doc.execute(SetTrack::new(id, TrackField::VolumeDb(*v))).unwrap();
        }
        prop_assert_eq!(doc.history().undo_len(), 1);
        prop_assert_eq!(doc.project().tracks[0].volume_db, *values.last().unwrap());
        doc.undo().unwrap();
        prop_assert_eq!(doc.project(), &start);
        doc.redo().unwrap();
        prop_assert_eq!(doc.project().tracks[0].volume_db, *values.last().unwrap());
    }
}
