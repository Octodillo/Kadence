//! Plays small projects through a session and checks what comes out.

use std::path::{Path, PathBuf};
use std::time::Duration;

use assert_no_alloc::{AllocDisabler, assert_no_alloc, violation_count};
use daw_core::event::EventList;
use daw_core::time::{PPQ, SampleTime, TickTime};
use daw_engine::{EngineConfig, EngineProcessor};
use daw_media::{AudioData, WavFormat, write_wav};
use daw_model::commands::{AddClip, AddTrack, ClipField, MoveClip, SetClip, SetTrack, TrackField};
use daw_model::{
    AudioClip, Clip, ClipContent, ClipId, Document, Project, Track, TrackId, TrackKind,
};
use daw_session::{Session, SessionConfig, effective_mutes};

#[global_allocator]
static ALLOC: AllocDisabler = AllocDisabler;

const RATE: f64 = 48_000.0;
const BLOCK: usize = 256;
/// Samples per beat at the default 120 bpm.
const BEAT: i64 = 24_000;

fn config() -> SessionConfig {
    SessionConfig {
        engine: EngineConfig {
            sample_rate: RATE,
            max_frames: BLOCK as u32,
            ..Default::default()
        },
        inputs: 0,
        outputs: 2,
    }
}

/// A mono file of `frames` samples that are all `value`.
fn dc_file(dir: &Path, name: &str, frames: usize, value: f32) -> PathBuf {
    let path = dir.join(name);
    let data = AudioData {
        sample_rate: RATE as u32,
        channels: vec![vec![value; frames]],
    };
    write_wav(&path, &data, WavFormat::Float32).unwrap();
    path
}

struct Rig {
    doc: Document,
    session: Session,
    proc: EngineProcessor,
    track: TrackId,
    _dir: tempfile::TempDir,
}

impl Rig {
    fn new(memory_limit: f64) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut doc = Document::new(Project::new("test"));
        let track = Track::new(TrackKind::Audio, "Audio 1");
        let id = track.id;
        doc.execute(AddTrack::new(0, track)).unwrap();
        let (mut session, proc) = Session::new(config());
        session.media_mut().set_memory_limit(memory_limit);
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

    fn dir(&self) -> &Path {
        self._dir.path()
    }

    fn sync(&mut self) {
        let snap = self.doc.snapshot();
        self.session.sync(&snap).unwrap();
    }

    fn add_clip(&mut self, file: &str, start_beats: i64, length_beats: i64) -> ClipId {
        let clip = Clip::new(
            "clip",
            TickTime(start_beats * PPQ),
            TickTime(length_beats * PPQ),
            ClipContent::Audio(AudioClip {
                source: file.into(),
                offset: SampleTime(0),
                gain_db: 0.0,
            }),
        );
        let id = clip.id;
        self.doc.execute(AddClip::new(self.track, clip)).unwrap();
        self.sync();
        id
    }

    /// Runs `frames` samples and returns the left channel.
    fn run(&mut self, frames: usize) -> Vec<f32> {
        let mut out = Vec::with_capacity(frames);
        let mut l = vec![0.0; BLOCK];
        let mut r = vec![0.0; BLOCK];
        let events = EventList::with_capacity(0);
        let mut done = 0;
        while done < frames {
            let n = BLOCK.min(frames - done);
            let mut outs: [&mut [f32]; 2] = [&mut l[..n], &mut r[..n]];
            let before = violation_count();
            assert_no_alloc(|| self.proc.process(n, &[], &mut outs, &events));
            assert_eq!(violation_count(), before, "process allocated");
            out.extend_from_slice(&l[..n]);
            done += n;
        }
        out
    }

    /// Like [`run`](Self::run), but at roughly real-time pace so the disk
    /// streamer keeps up the way it does with a real backend.
    fn run_paced(&mut self, frames: usize) -> Vec<f32> {
        let mut out = Vec::with_capacity(frames);
        while out.len() < frames {
            out.extend(self.run(BLOCK.min(frames - out.len())));
            std::thread::sleep(Duration::from_micros(500));
        }
        out
    }
}

#[test]
fn memory_clip_plays_at_its_position() {
    let mut rig = Rig::new(60.0);
    dc_file(rig.dir(), "a.wav", 48_000, 0.5);
    rig.add_clip("a.wav", 1, 1);
    rig.session.play().unwrap();
    let out = rig.run(3 * BEAT as usize);
    let beat = BEAT as usize;
    assert!(
        out[..beat].iter().all(|&x| x == 0.0),
        "silent before the clip"
    );
    // Past the edge fade the clip plays at full level.
    assert!((out[beat + 100] - 0.5).abs() < 1e-6, "{}", out[beat + 100]);
    assert!((out[2 * beat - 100] - 0.5).abs() < 1e-6);
    assert!(out[2 * beat..].iter().all(|&x| x == 0.0), "silent after");
}

#[test]
fn clip_and_fader_edits_do_not_rebuild_the_graph() {
    let mut rig = Rig::new(60.0);
    dc_file(rig.dir(), "a.wav", 48_000, 0.5);
    let clip = rig.add_clip("a.wav", 0, 1);
    rig.run(BLOCK);
    let swaps = rig.session.controller().graphs_swapped();

    rig.doc
        .execute(MoveClip::new(clip, rig.track, TickTime(2 * PPQ)))
        .unwrap();
    rig.doc
        .execute(SetClip::new(clip, ClipField::AudioGainDb(-6.0)))
        .unwrap();
    rig.doc
        .execute(SetTrack::new(rig.track, TrackField::VolumeDb(-6.0)))
        .unwrap();
    rig.sync();
    rig.session.play().unwrap();
    let out = rig.run(4 * BEAT as usize);
    assert_eq!(rig.session.controller().graphs_swapped(), swaps);

    let beat = BEAT as usize;
    assert!(out[..2 * beat].iter().all(|&x| x == 0.0), "moved away");
    let level = 0.5 * 10f32.powf(-12.0 / 20.0);
    assert!(
        (out[2 * beat + 200] - level).abs() < 1e-4,
        "{}",
        out[2 * beat + 200]
    );
}

#[test]
fn mute_and_solo_follow_the_model() {
    let mut rig = Rig::new(60.0);
    dc_file(rig.dir(), "a.wav", 48_000, 0.5);
    rig.add_clip("a.wav", 0, 1);
    let other = Track::new(TrackKind::Audio, "Audio 2");
    let other_id = other.id;
    rig.doc.execute(AddTrack::new(1, other)).unwrap();
    rig.doc
        .execute(SetTrack::new(other_id, TrackField::Solo(true)))
        .unwrap();
    let snap = rig.doc.snapshot();
    let muted = effective_mutes(&snap);
    assert!(muted.contains(&rig.track));
    assert!(!muted.contains(&other_id));
    rig.sync();
    rig.session.play().unwrap();
    assert!(rig.run(BEAT as usize).iter().all(|&x| x == 0.0));
}

#[test]
fn streamed_clip_loops_without_a_gap() {
    // Force streaming and loop the middle of a long clip, so every wrap has to
    // jump back in the file.
    let mut rig = Rig::new(0.0);
    dc_file(rig.dir(), "long.wav", 8 * BEAT as usize, 0.25);
    rig.add_clip("long.wav", 0, 8);
    rig.session
        .set_loop(Some((TickTime(2 * PPQ), TickTime(3 * PPQ))))
        .unwrap();
    rig.session.locate(TickTime(PPQ)).unwrap();
    // Let the streamer buffer the start position before playing.
    rig.run(BLOCK);
    std::thread::sleep(Duration::from_millis(200));
    rig.session.play().unwrap();
    // One beat to the loop, then three passes of the one-beat loop.
    let out = rig.run_paced(4 * BEAT as usize);
    let gaps = out.iter().filter(|&&x| x == 0.0).count();
    if let Some(first) = out.iter().position(|&x| x == 0.0) {
        eprintln!(
            "first gap at {first}, last at {:?}",
            out.iter().rposition(|&x| x == 0.0)
        );
    }
    assert_eq!(gaps, 0, "streamed playback had {gaps} silent samples");
}

#[test]
fn missing_file_is_a_warning_not_an_error() {
    let mut rig = Rig::new(60.0);
    rig.add_clip("nope.wav", 0, 1);
    assert!(
        rig.session
            .warnings()
            .iter()
            .any(|w| w.contains("nope.wav")),
        "{:?}",
        rig.session.warnings()
    );
    rig.session.play().unwrap();
    assert!(rig.run(BLOCK).iter().all(|&x| x == 0.0));
}

#[test]
fn adding_a_track_keeps_streams_playing() {
    // A structural edit rebuilds the graph; the running player (and its disk
    // readers) must carry over, so playback has no gap at the swap.
    let mut rig = Rig::new(0.0);
    dc_file(rig.dir(), "long.wav", 8 * BEAT as usize, 0.25);
    rig.add_clip("long.wav", 0, 8);
    rig.run(BLOCK);
    std::thread::sleep(Duration::from_millis(200));
    rig.session.play().unwrap();
    let mut out = rig.run_paced(BEAT as usize);
    let swaps = rig.session.controller().graphs_swapped();
    rig.doc
        .execute(AddTrack::new(1, Track::new(TrackKind::Audio, "Audio 2")))
        .unwrap();
    rig.sync();
    out.extend(rig.run_paced(BEAT as usize));
    assert!(
        rig.session.controller().graphs_swapped() > swaps,
        "graph was rebuilt"
    );
    // Skip the clip's fade-in at the very start.
    let gaps = out[100..].iter().filter(|&&x| x == 0.0).count();
    assert_eq!(gaps, 0, "{gaps} silent samples around the rebuild");
}

#[test]
fn strips_have_meters() {
    let mut rig = Rig::new(60.0);
    dc_file(rig.dir(), "a.wav", 48_000, 0.5);
    rig.add_clip("a.wav", 0, 1);
    rig.session.play().unwrap();
    rig.run(4 * BLOCK);
    let meter = rig.session.track_meter(rig.track).expect("track meter");
    let peaks = meter.take();
    assert!(peaks.iter().all(|&p| (p - 0.5).abs() < 0.01), "{peaks:?}");
    let master = rig.doc.project().master.id;
    assert!(rig.session.track_meter(master).is_some(), "master meter");
}

#[test]
fn volume_automation_plays() {
    use daw_model::commands::AddLane;
    use daw_model::{AutomationLane, AutomationPoint, AutomationTarget};
    let mut rig = Rig::new(60.0);
    dc_file(rig.dir(), "a.wav", 48_000, 0.5);
    rig.add_clip("a.wav", 0, 2);
    let mut lane = AutomationLane::new(AutomationTarget::Volume);
    lane.points = vec![
        AutomationPoint {
            tick: TickTime(0),
            value: 0.0,
            curve: 0.0,
            step: true,
        },
        AutomationPoint {
            tick: TickTime(PPQ),
            value: -6.0206,
            curve: 0.0,
            step: true,
        },
    ];
    rig.doc
        .execute(AddLane {
            track: rig.track,
            lane,
        })
        .unwrap();
    rig.sync();
    rig.session.play().unwrap();
    let out = rig.run(2 * BEAT as usize - 200);
    let beat = BEAT as usize;
    assert!((out[beat / 2] - 0.5).abs() < 1e-3, "{}", out[beat / 2]);
    assert!(
        (out[beat + beat / 2] - 0.25).abs() < 1e-3,
        "{}",
        out[beat + beat / 2]
    );
}
