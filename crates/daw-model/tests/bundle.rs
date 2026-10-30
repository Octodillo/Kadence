//! Bundle save/load, backups, autosave, migrations and load speed.

use std::time::Instant;

use daw_core::plugin::PluginFormatKind;
use daw_core::time::{SampleTime, TickTime};
use daw_model::bundle::{self, BACKUPS_KEPT, Migration};
use daw_model::commands::*;
use daw_model::*;
use serde_json::Value;

fn sample_project() -> Project {
    let mut p = Project::new("Song");
    let bus = Track::new(TrackKind::Bus, "Reverb");
    let mut audio = Track::new(TrackKind::Audio, "Vocals");
    audio.clips.push(
        Clip::new(
            "Take 1",
            TickTime(960),
            TickTime(3840),
            ClipContent::Audio(AudioClip {
                gain_db: -1.5,
                fade_in: Fade::new(TickTime(240), FadeShape::EqualPower),
                fade_out: Fade::new(TickTime(480), FadeShape::SCurve),
                ..AudioClip::new("audio/take1.wav", SampleTime(4410))
            }),
        )
        .into(),
    );
    let mut synth = Device::new(
        "Surge XT",
        DeviceSource::Plugin {
            format: PluginFormatKind::Clap,
            id: "org.surge-synth-team.surge-xt".into(),
        },
    );
    synth.params.insert(7, 0.1 + 0.2);
    synth.state = Some(vec![0, 1, 2, 255, 128].into());
    let mut inst = Track::new(TrackKind::Instrument, "Lead");
    let send = Send::new(bus.id);
    let mut lane = AutomationLane::new(AutomationTarget::DeviceParam {
        device: synth.id,
        param: 7,
    });
    lane.points.push(AutomationPoint {
        tick: TickTime(0),
        value: 1.0 / 3.0,
        curve: -0.25,
        step: false,
    });
    lane.points.push(AutomationPoint {
        tick: TickTime(1920),
        value: 0.9,
        curve: 0.0,
        step: true,
    });
    let mut send_lane = AutomationLane::new(AutomationTarget::SendLevel { send: send.id });
    send_lane.points.push(AutomationPoint {
        tick: TickTime(0),
        value: -6.0,
        curve: 0.0,
        step: false,
    });
    inst.devices.push(synth);
    inst.sends.push(send);
    inst.automation.extend([lane, send_lane]);
    let mut midi = MidiClip::default();
    for i in 0..16 {
        midi.notes.push(Note::new(
            TickTime(i * 240),
            TickTime(200),
            60 + (i % 12) as u8,
            0.75,
        ));
    }
    inst.clips
        .push(Clip::new("Riff", TickTime(0), TickTime(3840), ClipContent::Midi(midi)).into());
    p.tempo.tempos.push(TempoPoint {
        tick: TickTime(7680),
        bpm: 132.5,
        ramp: true,
    });
    p.tempo.meters.push(MeterPoint {
        tick: TickTime(7680),
        numerator: 7,
        denominator: 8,
    });
    p.tracks = vec![audio.into(), inst.into(), bus.into()];
    p.validate().unwrap();
    p
}

#[test]
fn json_round_trip_is_lossless() {
    let p = sample_project();
    let json = bundle::to_json(&p).unwrap();
    assert_eq!(bundle::from_json(&json).unwrap(), p);
    assert!(json.contains("\"format\": \"kadence-project\""));
}

#[test]
fn save_and_open_bundle() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("My Song");
    let mut doc = Document::new(sample_project());
    doc.save_as(&path).unwrap();
    assert!(path.join("My Song.kadence").is_file());
    assert!(path.join("audio").is_dir());
    assert!(path.join("backups").is_dir());
    assert!(!doc.is_modified());

    let reopened = Document::open(&path).unwrap();
    assert_eq!(reopened.project(), doc.project());
    let by_file = Document::open(path.join("My Song.kadence")).unwrap();
    assert_eq!(by_file.project(), doc.project());
}

#[test]
fn saves_keep_rotating_backups() {
    let dir = tempfile::tempdir().unwrap();
    let mut doc = Document::new(sample_project());
    doc.save_as(dir.path().join("B")).unwrap();
    let id = doc.project().tracks[0].id;
    for i in 0..BACKUPS_KEPT + 3 {
        doc.execute(SetTrack::new(id, TrackField::Name(format!("v{i}"))))
            .unwrap();
        doc.save().unwrap();
    }
    let backups = doc.bundle().unwrap().backups().unwrap();
    assert_eq!(backups.len(), BACKUPS_KEPT);
    // The newest backup is the save before last.
    let last =
        bundle::from_json(&std::fs::read_to_string(backups.last().unwrap()).unwrap()).unwrap();
    assert_eq!(last.tracks[0].name, format!("v{}", BACKUPS_KEPT + 1));
}

#[test]
fn autosave_and_recover() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("A");
    let mut doc = Document::new(sample_project());
    assert!(!doc.autosave().unwrap(), "no bundle yet");
    doc.save_as(&path).unwrap();
    assert!(!doc.autosave().unwrap(), "nothing changed");

    let id = doc.project().tracks[0].id;
    doc.execute(SetTrack::new(id, TrackField::Mute(true)))
        .unwrap();
    assert!(doc.autosave().unwrap());
    assert!(!doc.autosave().unwrap(), "already autosaved this revision");
    let bundle = doc.bundle().unwrap().clone();
    assert!(bundle.has_newer_autosave());

    let recovered = Document::recover(&path).unwrap();
    assert!(recovered.is_modified());
    assert_eq!(recovered.project(), doc.project());
    assert!(matches!(
        Document::open(&path),
        Err(FormatError::AutosavePending(_))
    ));

    doc.save().unwrap();
    assert!(!bundle.autosave_file().exists(), "save clears the autosave");
}

#[test]
fn modified_flag_follows_undo_past_save() {
    let mut doc = Document::new(sample_project());
    let id = doc.project().tracks[0].id;
    doc.execute(SetTrack::new(id, TrackField::VolumeDb(-3.0)))
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    doc.save_as(dir.path().join("M")).unwrap();
    // A fader drag after saving must not merge into the saved step.
    doc.execute(SetTrack::new(id, TrackField::VolumeDb(-4.0)))
        .unwrap();
    assert!(doc.is_modified());
    doc.undo().unwrap();
    assert!(!doc.is_modified());
    doc.undo().unwrap();
    assert!(doc.is_modified());
}

#[test]
fn import_audio_avoids_name_clashes() {
    let dir = tempfile::tempdir().unwrap();
    let b = Bundle::create(dir.path().join("I")).unwrap();
    let src = dir.path().join("kick.wav");
    std::fs::write(&src, b"RIFF").unwrap();
    assert_eq!(
        b.import_audio(&src).unwrap(),
        std::path::Path::new("audio/kick.wav")
    );
    assert_eq!(
        b.import_audio(&src).unwrap(),
        std::path::Path::new("audio/kick-1.wav")
    );
    assert!(b.resolve("audio/kick-1.wav".as_ref()).is_file());
}

#[test]
fn rejects_newer_and_foreign_files() {
    let json = bundle::to_json(&Project::default()).unwrap();
    let mut v: Value = serde_json::from_str(&json).unwrap();
    v["version"] = (SCHEMA_VERSION + 1).into();
    assert!(matches!(
        bundle::from_json(&v.to_string()),
        Err(FormatError::TooNew { .. })
    ));
    v["version"] = SCHEMA_VERSION.into();
    v["format"] = "something-else".into();
    assert!(matches!(
        bundle::from_json(&v.to_string()),
        Err(FormatError::WrongFormat(_))
    ));
}

#[test]
fn rejects_inconsistent_files() {
    let mut p = sample_project();
    let bus = p.tracks[2].id;
    // The bus routes into a track that sends to it: a loop.
    std::sync::Arc::make_mut(&mut p.tracks[2])
        .sends
        .push(Send::new(bus));
    let json = bundle::to_json(&p).unwrap();
    assert!(matches!(
        bundle::from_json(&json),
        Err(FormatError::Model(_))
    ));
}

#[test]
fn migrations_run_in_order() {
    // Pretend the current schema is 3 and the file is version 1, where the
    // project name was called "title" and sample rate was missing.
    fn v1_to_v2(mut v: Value) -> Result<Value, FormatError> {
        let title = v.as_object_mut().unwrap().remove("title").unwrap();
        v["name"] = title;
        Ok(v)
    }
    fn v2_to_v3(mut v: Value) -> Result<Value, FormatError> {
        v["sample_rate"] = 44_100.into();
        Ok(v)
    }
    const TABLE: &[Migration] = &[v1_to_v2, v2_to_v3];

    let mut v: Value =
        serde_json::from_str(&bundle::to_json(&Project::new("Old")).unwrap()).unwrap();
    let project = v["project"].as_object_mut().unwrap();
    let name = project.remove("name").unwrap();
    project.remove("sample_rate");
    project.insert("title".into(), name);
    v["version"] = 1.into();

    let p = bundle::from_json_with(&v.to_string(), 3, TABLE).unwrap();
    assert_eq!(p.name, "Old");
    assert_eq!(p.sample_rate, 44_100);
    assert!(matches!(
        bundle::from_json_with(&v.to_string(), 3, &TABLE[..1]),
        Err(FormatError::NoMigration(2))
    ));
}

/// `project` as an older build wrote it in version 1, with the given names.
fn v1_file(project: &Project, name: &str, master: &str) -> String {
    let mut v: Value = serde_json::from_str(&bundle::to_json(project).unwrap()).unwrap();
    v["version"] = 1.into();
    v["project"]["name"] = name.into();
    v["project"]["master"]["name"] = master.into();
    v.to_string()
}

#[test]
fn version_1_default_names_become_empty() {
    // New projects leave both names empty for the UI to translate.
    let p = Project::default();
    assert_eq!(p.name, "");
    assert_eq!(p.master.name, "");

    let song = sample_project();
    let p = bundle::from_json(&v1_file(&song, "Untitled", "Master")).unwrap();
    assert_eq!(p.name, "");
    assert_eq!(p.master.name, "");
    // Everything else survives the migration.
    assert_eq!(p.tracks, song.tracks);
    assert_eq!(p.master.id, song.master.id);

    // Names the user chose are kept, even ones close to the defaults.
    let p = bundle::from_json(&v1_file(&song, "Untitled 2", "master")).unwrap();
    assert_eq!(p.name, "Untitled 2");
    assert_eq!(p.master.name, "master");
    // Only the master's default name was English; a track called "Master" is
    // the user's.
    let mut v: Value = serde_json::from_str(&v1_file(&song, "Song", "Main")).unwrap();
    v["project"]["tracks"][0]["name"] = "Master".into();
    let p = bundle::from_json(&v.to_string()).unwrap();
    assert_eq!(p.name, "Song");
    assert_eq!(p.master.name, "Main");
    assert_eq!(p.tracks[0].name, "Master");
}

#[test]
fn snapshots_share_untouched_tracks() {
    let mut doc = Document::new(sample_project());
    let a = doc.snapshot();
    assert_eq!(doc.snapshot().revision(), a.revision());
    let id = doc.project().tracks[0].id;
    doc.execute(SetTrack::new(id, TrackField::Solo(true)))
        .unwrap();
    let b = doc.snapshot();
    assert!(b.revision() > a.revision());
    assert!(!a.tracks[0].solo, "old snapshot is unchanged");
    assert!(b.tracks[0].solo);
    assert!(std::sync::Arc::ptr_eq(&a.tracks[1], &b.tracks[1]));
    assert!(!std::sync::Arc::ptr_eq(&a.tracks[0], &b.tracks[0]));
}

/// The plan's target: a 1,000-track project loads in under a second.
#[test]
fn thousand_track_project_loads_fast() {
    let mut p = Project::new("Big");
    for i in 0..1000 {
        let mut t = Track::new(
            if i % 2 == 0 {
                TrackKind::Audio
            } else {
                TrackKind::Instrument
            },
            format!("Track {i}"),
        );
        for c in 0..4 {
            let start = TickTime(c * 3840);
            let content = if t.kind == TrackKind::Audio {
                ClipContent::Audio(AudioClip::new(format!("audio/{i}-{c}.wav"), SampleTime(0)))
            } else {
                let mut m = MidiClip::default();
                for n in 0..32 {
                    m.notes.push(Note::new(
                        TickTime(n * 120),
                        TickTime(100),
                        36 + (n % 48) as u8,
                        0.8,
                    ));
                }
                m.notes.sort_by_key(|n| (n.start, n.key, n.id));
                ClipContent::Midi(m)
            };
            t.clips
                .push(Clip::new("c", start, TickTime(3840), content).into());
        }
        let mut d = Device::new(
            "Gain",
            DeviceSource::Builtin {
                kind: "gain".into(),
            },
        );
        d.params.insert(0, -3.0);
        d.state = Some(vec![7u8; 256].into());
        let mut lane = AutomationLane::new(AutomationTarget::Volume);
        lane.points = (0..16)
            .map(|k| AutomationPoint {
                tick: TickTime(k * 960),
                value: -(k as f64),
                curve: 0.0,
                step: false,
            })
            .collect();
        t.devices.push(d);
        t.automation.push(lane);
        p.tracks.push(t.into());
    }
    let dir = tempfile::tempdir().unwrap();
    let b = Bundle::create(dir.path().join("Big")).unwrap();
    b.save(&p).unwrap();

    let started = Instant::now();
    let loaded = b.load().unwrap();
    let took = started.elapsed();
    assert_eq!(loaded, p);
    let size = std::fs::metadata(b.project_file()).unwrap().len();
    eprintln!("1,000 tracks: {size} bytes, loaded in {took:?}");
    assert!(took.as_secs_f64() < 1.0, "load took {took:?}");
}

#[test]
fn older_tracks_get_default_arm_and_midi_input() {
    let mut song = sample_project();
    let mut v: Value = serde_json::from_str(&bundle::to_json(&song).unwrap()).unwrap();
    v["version"] = 2.into();
    for t in v["project"]["tracks"].as_array_mut().unwrap() {
        let t = t.as_object_mut().unwrap();
        t.remove("armed");
        t.remove("midi_input");
    }
    let p = bundle::from_json(&v.to_string()).unwrap();
    assert_eq!(p.tracks, song.tracks);
    assert!(
        p.tracks
            .iter()
            .all(|t| !t.armed && t.midi_input == MidiInput::default())
    );

    // Both round-trip.
    let t = std::sync::Arc::make_mut(&mut song.tracks[0]);
    t.armed = true;
    t.midi_input = MidiInput {
        device: Some("Midi-Bridge:Keys (capture)".into()),
        channel: Some(9),
    };
    let p = bundle::from_json(&bundle::to_json(&song).unwrap()).unwrap();
    assert_eq!(p.tracks, song.tracks);

    // A channel out of range is refused on load.
    let mut v: Value = serde_json::from_str(&bundle::to_json(&song).unwrap()).unwrap();
    v["project"]["tracks"][0]["midi_input"]["channel"] = 16.into();
    assert!(bundle::from_json(&v.to_string()).is_err());
}

#[test]
fn arming_and_midi_input_are_undoable_track_edits() {
    let mut doc = Document::new(sample_project());
    let id = doc.project().tracks[0].id;
    doc.execute(SetTrack::new(id, TrackField::Armed(true)))
        .unwrap();
    assert_eq!(doc.history().undo_label().as_deref(), Some("Arm Track"));
    let input = MidiInput {
        device: None,
        channel: Some(0),
    };
    doc.execute(SetTrack::new(id, TrackField::MidiInput(input.clone())))
        .unwrap();
    assert_eq!(
        doc.history().undo_label().as_deref(),
        Some("Change MIDI Input")
    );
    let track = doc.project().track(id).unwrap();
    assert!(track.armed);
    assert_eq!(track.midi_input, input);
    assert!(
        doc.execute(SetTrack::new(
            id,
            TrackField::MidiInput(MidiInput {
                device: None,
                channel: Some(16),
            })
        ))
        .is_err()
    );
    doc.undo().unwrap();
    doc.undo().unwrap();
    let track = doc.project().track(id).unwrap();
    assert!(!track.armed);
    assert_eq!(track.midi_input, MidiInput::default());
}
