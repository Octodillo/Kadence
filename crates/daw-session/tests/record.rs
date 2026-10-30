//! Records audio through the dummy backend's loopback, which plays the
//! outputs back into the inputs late by the latency it reports, and checks
//! that the take lands where the played audio was.

use std::path::Path;
use std::time::{Duration, Instant};

use daw_backend::{AudioBackend, BackendConfig, DummyBackend, Latency, Pacing};
use daw_core::time::{PPQ, SampleTime, TickTime};
use daw_engine::EngineConfig;
use daw_media::{AudioData, WavFormat, decode_file, write_wav};
use daw_model::commands::{AddClip, AddTrack, SetTrack, TrackField};
use daw_model::{
    AudioClip, AudioInput, Clip, ClipContent, Document, InputChannels, Monitoring, Project, Track,
    TrackId, TrackKind,
};
use daw_session::record::AudioTakes;
use daw_session::{Session, SessionConfig};

const RATE: u32 = 48_000;
const PERIOD: u32 = 256;
/// Where the impulses are in the source file, and how loud.
const IMPULSES: [(usize, f32); 3] = [(1_000, 0.9), (7_321, 0.6), (15_007, 0.3)];

struct Rig {
    doc: Document,
    session: Session,
    backend: DummyBackend,
    mic: TrackId,
    dir: tempfile::TempDir,
}

fn rig(latency: Latency) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let mut source = vec![0.0f32; 20_000];
    for (at, v) in IMPULSES {
        source[at] = v;
    }
    let data = AudioData {
        sample_rate: RATE,
        channels: vec![source],
    };
    write_wav(dir.path().join("source.wav"), &data, WavFormat::Float32).unwrap();

    let mut doc = Document::new(Project::new("rec"));
    let player = Track::new(TrackKind::Audio, "Source");
    let player_id = player.id;
    doc.execute(AddTrack::new(0, player)).unwrap();
    // One beat in: some silence before the clip, on a tick that is a whole
    // number of samples.
    let clip = Clip::new(
        "source",
        TickTime(PPQ),
        TickTime(4 * PPQ),
        ClipContent::Audio(AudioClip {
            source: "source.wav".into(),
            offset: SampleTime(0),
            gain_db: 0.0,
        }),
    );
    doc.execute(AddClip::new(player_id, clip)).unwrap();
    let mic = Track::new(TrackKind::Audio, "Mic");
    let mic_id = mic.id;
    doc.execute(AddTrack::new(1, mic)).unwrap();
    doc.execute(SetTrack::new(mic_id, TrackField::Armed(true)))
        .unwrap();
    doc.execute(SetTrack::new(
        mic_id,
        TrackField::AudioInput(AudioInput {
            channels: InputChannels::Mono(0),
            monitor: Monitoring::Off,
        }),
    ))
    .unwrap();
    doc.seal();

    let mut backend = DummyBackend::new(Pacing::Freewheel).with_loopback(latency);
    backend
        .open(BackendConfig {
            client_name: "rec".into(),
            audio_inputs: 2,
            audio_outputs: 2,
            midi_input: false,
            midi_output: false,
            sample_rate: Some(RATE),
            buffer_size: Some(PERIOD),
            auto_connect: true,
            midi_capacity: 16,
        })
        .unwrap();
    let (mut session, mut processor) = Session::new(SessionConfig {
        engine: EngineConfig {
            sample_rate: f64::from(RATE),
            max_frames: PERIOD,
            ..Default::default()
        },
        inputs: 2,
        outputs: 2,
    });
    session.set_root(Some(dir.path().to_path_buf()));
    session.sync(&doc.snapshot()).unwrap();
    backend
        .start(Box::new(move |c| {
            processor.process(c.frames as usize, c.audio_in, c.audio_out, c.midi_in)
        }))
        .unwrap();
    Rig {
        doc,
        session,
        backend,
        mic: mic_id,
        dir,
    }
}

/// Waits for the transport to pass `samples`.
fn wait_past(session: &Session, samples: i64) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while session.controller().position().0 < samples {
        assert!(Instant::now() < deadline, "the transport didn't get there");
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// The timeline positions (in samples) and values of the impulses in an
/// audio clip on `track`.
fn impulses_on(rig: &Rig, track: TrackId) -> Vec<(i64, f32)> {
    let project = rig.doc.project();
    let clip = &project.track(track).unwrap().clips[0];
    let ClipContent::Audio(a) = &clip.content else {
        panic!("not an audio clip");
    };
    let path = if a.source.is_absolute() {
        a.source.clone()
    } else {
        rig.dir.path().join(&a.source)
    };
    let data = decode_file(&path).unwrap();
    assert_eq!(data.channels.len(), 1, "a mono input makes a mono take");
    let start = rig.session.ticks_to_samples(clip.start).0;
    let end = rig
        .session
        .ticks_to_samples(TickTime(clip.start.0 + clip.length.0))
        .0;
    data.channels[0]
        .iter()
        .enumerate()
        .skip(a.offset.0 as usize)
        .filter(|(_, v)| v.abs() > 0.05)
        .map(|(i, &v)| (start + i as i64 - a.offset.0, v))
        .filter(|&(t, _)| t < end)
        .collect()
}

fn record_and_check(latency: Latency) {
    let mut rig = rig(latency);
    assert_eq!(rig.backend.latency().total(), latency.total());
    let (mut takes, problems) = AudioTakes::start(
        &mut rig.session,
        rig.doc.project(),
        &rig.dir.path().join("audio"),
        rig.backend.latency().total(),
    );
    assert!(problems.is_empty(), "{problems:?}");
    assert_eq!(takes.tracks().collect::<Vec<_>>(), [rig.mic]);
    rig.session.play().unwrap();
    let beat = i64::from(RATE) / 2;
    wait_past(
        &rig.session,
        beat + 20_000 + i64::from(latency.total()) + 4096,
    );
    assert!(takes.poll().is_empty());
    let finished = takes.finish(&mut rig.session, rig.doc.project(), Some(rig.dir.path()));
    rig.session.stop().unwrap();
    rig.backend.stop().unwrap();
    assert!(finished.problems.is_empty(), "{:?}", finished.problems);
    let cmd = finished.command.expect("a take");
    rig.doc.execute_boxed(cmd).unwrap();
    rig.doc.seal();
    assert_eq!(
        rig.doc.history().undo_label().as_deref(),
        Some("Record Audio")
    );

    let clips = &rig.doc.project().track(rig.mic).unwrap().clips;
    assert_eq!(clips.len(), 1);
    let ClipContent::Audio(a) = &clips[0].content else {
        panic!()
    };
    assert!(a.source.starts_with("audio"), "{:?}", a.source);
    assert!(Path::new(&rig.dir.path().join(&a.source)).exists());

    // The source plays impulse i at beat + IMPULSES[i].0; the take must
    // have it at the same timeline position, within a sample.
    let got = impulses_on(&rig, rig.mic);
    assert_eq!(got.len(), IMPULSES.len(), "{got:?}");
    for ((t, v), (at, want)) in got.iter().zip(IMPULSES) {
        let expected = beat + at as i64;
        assert!(
            (t - expected).abs() <= 1,
            "impulse at {t}, played at {expected} (latency {latency:?})"
        );
        // The centre-panned strip scales it, but the ratio between
        // impulses survives.
        let ratio = v / got[0].1;
        assert!((ratio - want / IMPULSES[0].1).abs() < 1e-3, "{ratio}");
    }
    assert_eq!(rig.backend.stats().rt_alloc_violations, 0);

    // Undo takes the take off the track.
    rig.doc.undo().unwrap();
    assert!(rig.doc.project().track(rig.mic).unwrap().clips.is_empty());
}

#[test]
fn a_take_lines_up_with_what_was_played() {
    record_and_check(Latency {
        input: 300,
        output: 200,
    });
}

#[test]
fn a_take_lines_up_with_odd_latencies() {
    // Not a multiple of the period nor of a tick (25 samples at 120 bpm).
    record_and_check(Latency {
        input: 263,
        output: 1_337,
    });
}

#[test]
fn only_armed_audio_tracks_record() {
    let mut rig = rig(Latency {
        input: 300,
        output: 300,
    });
    rig.doc
        .execute(SetTrack::new(rig.mic, TrackField::Armed(false)))
        .unwrap();
    rig.session.sync(&rig.doc.snapshot()).unwrap();
    let (takes, problems) = AudioTakes::start(
        &mut rig.session,
        rig.doc.project(),
        &rig.dir.path().join("audio"),
        600,
    );
    assert!(problems.is_empty());
    assert!(takes.is_empty());
}

#[test]
fn discarding_leaves_no_files() {
    let mut rig = rig(Latency {
        input: 300,
        output: 300,
    });
    let audio = rig.dir.path().join("audio");
    let (takes, _) = AudioTakes::start(&mut rig.session, rig.doc.project(), &audio, 600);
    rig.session.play().unwrap();
    wait_past(&rig.session, 10_000);
    takes.discard(&mut rig.session);
    rig.backend.stop().unwrap();
    assert_eq!(std::fs::read_dir(&audio).unwrap().count(), 0);
}

#[test]
fn auto_monitoring_follows_arm_and_transport() {
    let mut rig = rig(Latency {
        input: 300,
        output: 300,
    });
    assert!(!rig.session.is_monitoring(rig.mic), "monitoring is off");
    rig.doc
        .execute(SetTrack::new(
            rig.mic,
            TrackField::AudioInput(AudioInput {
                channels: InputChannels::Mono(0),
                monitor: Monitoring::Auto,
            }),
        ))
        .unwrap();
    rig.session.sync(&rig.doc.snapshot()).unwrap();
    assert!(rig.session.is_monitoring(rig.mic), "armed and stopped");
    rig.session.play().unwrap();
    assert!(
        !rig.session.is_monitoring(rig.mic),
        "playing back without recording"
    );
    let (takes, _) = AudioTakes::start(
        &mut rig.session,
        rig.doc.project(),
        &rig.dir.path().join("audio"),
        600,
    );
    assert!(rig.session.is_monitoring(rig.mic), "recording");
    takes.discard(&mut rig.session);
    assert!(!rig.session.is_monitoring(rig.mic));
    rig.session.stop().unwrap();
    rig.doc
        .execute(SetTrack::new(rig.mic, TrackField::Armed(false)))
        .unwrap();
    rig.session.sync(&rig.doc.snapshot()).unwrap();
    assert!(!rig.session.is_monitoring(rig.mic), "disarmed");
    rig.backend.stop().unwrap();
}
