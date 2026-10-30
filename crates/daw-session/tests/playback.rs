//! Plays small projects through a session and checks what comes out.

use std::path::{Path, PathBuf};
use std::time::Duration;

use assert_no_alloc::{AllocDisabler, assert_no_alloc, violation_count};
use daw_core::event::EventList;
use daw_core::time::{PPQ, SampleTime, TempoMap, TickTime};
use daw_engine::{EngineConfig, EngineProcessor};
use daw_media::{AudioData, WavFormat, write_wav};
use daw_model::commands::{
    AddClip, AddTrack, ClipField, MoveClip, SetClip, SetTempo, SetTrack, TrackField,
};
use daw_model::{
    AudioClip, Clip, ClipContent, ClipId, Document, Fade, FadeShape, Project, Track, TrackId,
    TrackKind,
};
use daw_session::{Session, SessionConfig, Warning, effective_mutes};

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
            ClipContent::Audio(AudioClip::new(file, SampleTime(0))),
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
            .any(|w| matches!(w, Warning::CannotRead { path } if path.ends_with("nope.wav"))),
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

#[test]
fn stream_edited_while_stopped_plays_without_gap() {
    let mut rig = Rig::new(0.0);
    dc_file(rig.dir(), "long.wav", 16 * BEAT as usize, 0.25);
    let clip = rig.add_clip("long.wav", 0, 8);
    rig.session.locate(TickTime(2 * PPQ)).unwrap();
    rig.run(BLOCK);
    std::thread::sleep(Duration::from_millis(200));
    // Move the clip half a beat left while stopped; it still covers the playhead.
    rig.doc
        .execute(MoveClip::new(clip, rig.track, TickTime(0)))
        .unwrap();
    rig.doc
        .execute(SetClip::new(
            clip,
            ClipField::AudioOffset(SampleTime(BEAT / 2)),
        ))
        .unwrap();
    rig.sync();
    rig.run(BLOCK);
    std::thread::sleep(Duration::from_millis(300));
    rig.run(BLOCK);
    std::thread::sleep(Duration::from_millis(300));
    rig.session.play().unwrap();
    let out = rig.run_paced(BEAT as usize / 2);
    let gaps = out.iter().filter(|&&x| x == 0.0).count();
    assert_eq!(
        gaps, 0,
        "{gaps} silent samples after play following a stopped edit"
    );
}

#[test]
fn loop_wraps_cleanly_after_offset_edit() {
    let mut rig = Rig::new(0.0);
    dc_file(rig.dir(), "long.wav", 16 * BEAT as usize, 0.25);
    let clip = rig.add_clip("long.wav", 0, 8);
    rig.session
        .set_loop(Some((TickTime(2 * PPQ), TickTime(3 * PPQ))))
        .unwrap();
    rig.session.locate(TickTime(2 * PPQ)).unwrap();
    rig.run(BLOCK);
    std::thread::sleep(Duration::from_millis(200));
    rig.session.play().unwrap();
    let _ = rig.run_paced(BEAT as usize / 4);
    // Change the file offset while playing (like a slip edit); the clip still spans the loop start.
    rig.doc
        .execute(SetClip::new(clip, ClipField::AudioOffset(SampleTime(BEAT))))
        .unwrap();
    rig.sync();
    // The edit itself re-seeks the playing reader; only the wraps after it
    // must be clean.
    let _ = rig.run_paced(BEAT as usize / 2);
    let rest = rig.run_paced(3 * BEAT as usize); // crosses several wraps
    let gaps = rest.iter().filter(|&&x| x == 0.0).count();
    assert_eq!(
        gaps, 0,
        "{gaps} silent samples at loop wraps after the edit"
    );
}

#[test]
fn stream_parked_then_played_has_no_gap() {
    let mut rig = Rig::new(0.0);
    dc_file(rig.dir(), "long.wav", 16 * BEAT as usize, 0.25);
    let _clip = rig.add_clip("long.wav", 0, 8);
    rig.session.locate(TickTime(2 * PPQ)).unwrap();
    rig.run(BLOCK);
    std::thread::sleep(Duration::from_millis(200));
    rig.run(BLOCK);
    std::thread::sleep(Duration::from_millis(300));
    rig.session.play().unwrap();
    let out = rig.run_paced(BEAT as usize / 2);
    let gaps = out.iter().filter(|&&x| x == 0.0).count();
    assert_eq!(gaps, 0, "{gaps} control");
}

#[test]
fn setting_the_same_root_keeps_the_graph() {
    let mut rig = Rig::new(60.0);
    dc_file(rig.dir(), "a.wav", 48_000, 0.5);
    let clip = rig.add_clip("a.wav", 0, 1);
    rig.run(BLOCK);
    let swaps = rig.session.controller().graphs_swapped();
    rig.doc
        .execute(MoveClip::new(clip, rig.track, TickTime(2 * PPQ)))
        .unwrap();
    // What AppState::sync does on every document change:
    let root = rig.dir().to_path_buf();
    rig.session.set_root(Some(root));
    rig.sync();
    rig.run(BLOCK);
    assert_eq!(
        rig.session.controller().graphs_swapped(),
        swaps,
        "a clip move rebuilt the graph"
    );
}

#[test]
fn split_clip_plays_through_the_cut_without_a_dip() {
    let mut rig = Rig::new(60.0);
    dc_file(rig.dir(), "pad.wav", 4 * BEAT as usize, 0.5);
    // The two halves of a split: the right one starts where the left ends,
    // two beats into the file.
    let left = rig.add_clip("pad.wav", 0, 2);
    let right = rig.add_clip("pad.wav", 2, 2);
    rig.doc
        .execute(SetClip::new(
            right,
            ClipField::AudioOffset(SampleTime(2 * BEAT)),
        ))
        .unwrap();
    rig.sync();
    assert!(rig.session.has_clip(rig.track, left));
    rig.session.play().unwrap();
    let out = rig.run(3 * BEAT as usize);
    let level = out[BEAT as usize];
    let cut = 2 * BEAT as usize;
    for (i, &x) in out[cut - 128..cut + 128].iter().enumerate() {
        assert!(
            (x - level).abs() < 1e-6,
            "dip at {}: {x} vs {level}",
            cut - 128 + i
        );
    }
}

#[test]
fn tempo_and_meter_edits_reach_the_engine() {
    let mut rig = Rig::new(f64::INFINITY);
    let mut tempo = rig.doc.project().tempo.clone();
    tempo.tempos[0].bpm = 90.0;
    tempo.meters[0].numerator = 3;
    rig.doc.execute(SetTempo { tempo }).unwrap();
    rig.sync();
    let timeline = rig.session.controller().timeline();
    assert_eq!(timeline.tempo.bpm_at(TickTime(0)), 90.0);
    assert_eq!(timeline.meter.points()[0].numerator, 3);
}

#[test]
fn live_params_hold_over_syncs_and_suspended_lanes_are_skipped() {
    use daw_engine::nodes::Strip;
    use daw_model::commands::AddLane;
    use daw_model::{AutomationLane, AutomationPoint, AutomationTarget};
    let mut rig = Rig::new(60.0);
    dc_file(rig.dir(), "a.wav", 48_000 * 4, 0.5);
    rig.add_clip("a.wav", 0, 8);
    let mut lane = AutomationLane::new(AutomationTarget::Volume);
    lane.points = vec![AutomationPoint {
        tick: TickTime(0),
        value: -6.0206,
        curve: 0.0,
        step: true,
    }];
    let lane_id = lane.id;
    rig.doc
        .execute(AddLane {
            track: rig.track,
            lane,
        })
        .unwrap();
    rig.sync();
    rig.session.play().unwrap();
    let level = |rig: &mut Rig| *rig.run(BLOCK * 4).last().unwrap();
    assert!((level(&mut rig) - 0.25).abs() < 1e-3, "the lane plays");

    // A held fader: its lane is left out and its value stays live over a
    // sync caused by another edit.
    let key = rig.session.strip_key(rig.track).unwrap();
    rig.session.set_suspended_lanes([lane_id].into());
    rig.session
        .set_live_param(key, Strip::GAIN, Some(0.2))
        .unwrap();
    rig.sync();
    assert!((level(&mut rig) - 0.1).abs() < 1e-3);
    rig.doc
        .execute(SetTrack::new(rig.track, TrackField::Name("Renamed".into())))
        .unwrap();
    rig.sync();
    assert!((level(&mut rig) - 0.1).abs() < 1e-3, "held over the sync");

    // Let go: the document's static value, then the lane again.
    rig.session.set_live_param(key, Strip::GAIN, None).unwrap();
    rig.sync();
    assert!((level(&mut rig) - 0.5).abs() < 1e-3);
    rig.session.set_suspended_lanes(Default::default());
    rig.sync();
    assert!((level(&mut rig) - 0.25).abs() < 1e-3);
}

/// An instrument track with the built-in synth and one MIDI clip of a
/// held note over beats 1 to 3.
fn synth_rig() -> (Rig, TrackId, ClipId) {
    let mut rig = Rig::new(f64::INFINITY);
    let track = Track::new(TrackKind::Instrument, "Synth");
    let tid = track.id;
    rig.doc.execute(AddTrack::new(1, track)).unwrap();
    let synth = daw_model::Device::new(
        "Synth",
        daw_model::DeviceSource::Builtin {
            kind: "daw.synth".into(),
        },
    );
    rig.doc
        .execute(daw_model::commands::AddDevice {
            track: tid,
            index: 0,
            device: synth,
        })
        .unwrap();
    let note = daw_model::Note {
        id: daw_model::NoteUid::new(),
        start: TickTime(0),
        length: TickTime(2 * PPQ),
        channel: 0,
        key: 60,
        velocity: 0.8,
    };
    let clip = Clip::new(
        "notes",
        TickTime(PPQ),
        TickTime(4 * PPQ),
        ClipContent::Midi(daw_model::MidiClip { notes: vec![note] }),
    );
    let cid = clip.id;
    rig.doc.execute(AddClip::new(tid, clip)).unwrap();
    rig.sync();
    (rig, tid, cid)
}

fn peak(s: &[f32]) -> f32 {
    s.iter().fold(0.0, |m, v| m.max(v.abs()))
}

#[test]
fn midi_clip_plays_its_notes_through_the_instrument() {
    let (mut rig, _, _) = synth_rig();
    rig.session.play().unwrap();
    let out = rig.run(4 * BEAT as usize);
    assert!(peak(&out[..20_000]) < 1e-4, "silent before the clip");
    assert!(peak(&out[30_000..70_000]) > 1e-3, "the note sounds");
}

#[test]
fn note_edits_reach_the_player_without_a_rebuild() {
    let (mut rig, _, clip) = synth_rig();
    rig.run(BLOCK);
    let swaps = rig.session.controller().graphs_swapped();
    let mut notes = match &rig.doc.project().clip(clip).unwrap().content {
        ClipContent::Midi(m) => m.notes.clone(),
        _ => unreachable!(),
    };
    notes[0].start = TickTime(2 * PPQ);
    rig.doc
        .execute(daw_model::commands::EditNotes { clip, notes })
        .unwrap();
    rig.sync();
    rig.session.play().unwrap();
    let out = rig.run(4 * BEAT as usize);
    assert_eq!(
        rig.session.controller().graphs_swapped(),
        swaps,
        "no rebuild"
    );
    assert!(peak(&out[..70_000]) < 1e-4, "the note moved later");
    assert!(peak(&out[74_000..]) > 1e-3, "and plays there");
}

#[test]
fn switching_the_instrument_off_and_on_leaves_no_hanging_note() {
    let (mut rig, track, _) = synth_rig();
    rig.session.play().unwrap();
    rig.run(2 * BEAT as usize); // Inside the held note.
    let device = rig.doc.project().track(track).unwrap().devices[0].id;
    let set_enabled = |rig: &mut Rig, on: bool| {
        rig.doc
            .execute(daw_model::commands::SetDevice {
                id: device,
                field: daw_model::commands::DeviceField::Enabled(on),
            })
            .unwrap();
        rig.sync();
    };
    set_enabled(&mut rig, false);
    // The note ends at beat 3 while the synth is off.
    rig.run(2 * BEAT as usize);
    rig.session.stop().unwrap();
    set_enabled(&mut rig, true);
    let out = rig.run(BEAT as usize);
    assert!(
        peak(&out[BEAT as usize / 2..]) < 1e-4,
        "no voice left sounding"
    );
}

/// Gain of a fade `d` samples into a fade of `len` samples, as the player
/// samples it (at the middle of each sample).
fn fade_at(shape: FadeShape, d: i64, len: i64) -> f32 {
    shape.gain((d as f32 + 0.5) / len as f32)
}

fn set_fades(rig: &mut Rig, clip: ClipId, fade_in: Fade, fade_out: Fade) {
    rig.doc
        .execute(SetClip::new(clip, ClipField::FadeIn(fade_in)))
        .unwrap();
    rig.doc
        .execute(SetClip::new(clip, ClipField::FadeOut(fade_out)))
        .unwrap();
    rig.sync();
}

#[test]
fn fades_and_clip_gain_shape_a_memory_clip() {
    let mut rig = Rig::new(60.0);
    dc_file(rig.dir(), "a.wav", 2 * BEAT as usize, 0.5);
    let clip = rig.add_clip("a.wav", 0, 2);
    let swaps = rig.session.controller().graphs_swapped();
    // Half a beat of linear fade-in, half a beat of equal-power fade-out.
    set_fades(
        &mut rig,
        clip,
        Fade::new(TickTime(PPQ / 2), FadeShape::Linear),
        Fade::new(TickTime(PPQ / 2), FadeShape::EqualPower),
    );
    rig.doc
        .execute(SetClip::new(clip, ClipField::AudioGainDb(-6.0)))
        .unwrap();
    rig.sync();
    assert_eq!(
        rig.session.controller().graphs_swapped(),
        swaps,
        "fade and gain edits reach the player without a graph rebuild"
    );
    rig.session.play().unwrap();
    let out = rig.run(3 * BEAT as usize);

    let level = 0.5 * 10f32.powf(-6.0 / 20.0);
    let half = BEAT / 2;
    for d in [100, half / 2, half - 1] {
        let want = level * fade_at(FadeShape::Linear, d, half);
        assert!(
            (out[d as usize] - want).abs() < 1e-4,
            "fade-in at {d}: {} vs {want}",
            out[d as usize]
        );
    }
    assert!((out[(half + 10) as usize] - level).abs() < 1e-5);
    assert!((out[(2 * BEAT - half - 10) as usize] - level).abs() < 1e-5);
    let end = 2 * BEAT;
    for d in [100, half / 2, half - 1] {
        let at = (end - 1 - d) as usize;
        let want = level * fade_at(FadeShape::EqualPower, d, half);
        assert!(
            (out[at] - want).abs() < 1e-4,
            "fade-out {d} before the end: {} vs {want}",
            out[at]
        );
    }
    // Rises and falls without a step anywhere.
    for w in out[..end as usize].windows(2) {
        assert!((w[1] - w[0]).abs() < 0.01, "{} -> {}", w[0], w[1]);
    }
}

#[test]
fn fades_longer_than_the_clip_shrink_together() {
    let mut rig = Rig::new(60.0);
    dc_file(rig.dir(), "a.wav", 2 * BEAT as usize, 0.5);
    let clip = rig.add_clip("a.wav", 0, 2);
    // Two beats each on a two-beat clip (as after a trim): one beat each.
    set_fades(
        &mut rig,
        clip,
        Fade::new(TickTime(2 * PPQ), FadeShape::Linear),
        Fade::new(TickTime(2 * PPQ), FadeShape::Linear),
    );
    rig.session.play().unwrap();
    let out = rig.run(2 * BEAT as usize);
    let mid = BEAT / 2;
    let want = 0.5 * fade_at(FadeShape::Linear, mid, BEAT);
    assert!(
        (out[mid as usize] - want).abs() < 1e-4,
        "{}",
        out[mid as usize]
    );
    let peak = out.iter().copied().fold(0.0f32, f32::max);
    assert!(
        peak > 0.49 && peak <= 0.5,
        "peaks where the fades meet: {peak}"
    );
}

#[test]
fn fades_shape_a_streamed_clip() {
    let mut rig = Rig::new(0.0);
    dc_file(rig.dir(), "long.wav", 4 * BEAT as usize, 0.25);
    let clip = rig.add_clip("long.wav", 1, 2);
    set_fades(
        &mut rig,
        clip,
        Fade::new(TickTime(PPQ), FadeShape::SCurve),
        Fade::default(),
    );
    rig.run(BLOCK);
    std::thread::sleep(Duration::from_millis(200));
    rig.session.play().unwrap();
    let out = rig.run_paced(3 * BEAT as usize);
    let b = BEAT as usize;
    assert!(out[..b].iter().all(|&x| x == 0.0));
    for d in [BEAT / 4, BEAT / 2, 3 * BEAT / 4] {
        let want = 0.25 * fade_at(FadeShape::SCurve, d, BEAT);
        let got = out[b + d as usize];
        assert!((got - want).abs() < 1e-4, "{d}: {got} vs {want}");
    }
    assert!((out[2 * b + 100] - 0.25).abs() < 1e-5);
}

/// An audio track playing a steady 0.5 through `kinds` (built-in device
/// ids), in that order. Returns the rig and the devices' ids.
fn effect_rig(kinds: &[&str]) -> (Rig, Vec<daw_model::DeviceId>) {
    let mut rig = Rig::new(60.0);
    dc_file(rig.dir(), "a.wav", 8 * BEAT as usize, 0.5);
    rig.add_clip("a.wav", 0, 8);
    let mut ids = Vec::new();
    for (i, kind) in kinds.iter().enumerate() {
        let device = daw_model::Device::new(
            *kind,
            daw_model::DeviceSource::Builtin {
                kind: (*kind).into(),
            },
        );
        ids.push(device.id);
        rig.doc
            .execute(daw_model::commands::AddDevice {
                track: rig.track,
                index: i,
                device,
            })
            .unwrap();
    }
    rig.sync();
    rig.session.play().unwrap();
    (rig, ids)
}

/// The level the track settles at: the last sample of a beat's run.
fn settled(rig: &mut Rig) -> f32 {
    *rig.run(BEAT as usize).last().unwrap()
}

#[test]
fn device_param_edits_play_without_a_rebuild_and_reset_to_default() {
    use daw_devices::devices::eq;
    use daw_model::commands::SetParam;
    let (mut rig, ids) = effect_rig(&["daw.eq"]);
    let eq = ids[0];
    let before = settled(&mut rig);
    assert!((before - 0.5).abs() < 1e-3, "{before}");
    let swaps = rig.session.controller().graphs_swapped();

    let set = |rig: &mut Rig, value: Option<f64>| {
        rig.doc
            .execute(SetParam {
                device: eq,
                param: eq::OUTPUT,
                value,
            })
            .unwrap();
        rig.sync();
    };
    set(&mut rig, Some(-6.0));
    // A separate gesture, so undo doesn't merge it with the reset.
    rig.doc.seal();
    let cut = settled(&mut rig);
    assert!((cut - 0.5 * 10f32.powf(-6.0 / 20.0)).abs() < 1e-3, "{cut}");

    // Back to the default (no value in the document): heard at once.
    set(&mut rig, None);
    let reset = settled(&mut rig);
    assert!((reset - before).abs() < 1e-4, "{reset}");

    // Undo brings the edit back the same way.
    rig.doc.undo().unwrap();
    rig.sync();
    let undone = settled(&mut rig);
    assert!((undone - cut).abs() < 1e-4, "{undone}");
    assert_eq!(
        rig.session.controller().graphs_swapped(),
        swaps,
        "parameter edits don't rebuild the graph"
    );
}

#[test]
fn compressor_threshold_changes_the_output() {
    use daw_devices::devices::compressor;
    let (mut rig, ids) = effect_rig(&["daw.compressor"]);
    // 0.5 is about -6 dBFS, above the default -18 dB threshold.
    let squeezed = settled(&mut rig);
    rig.doc
        .execute(daw_model::commands::SetParam {
            device: ids[0],
            param: compressor::THRESHOLD,
            value: Some(0.0),
        })
        .unwrap();
    rig.sync();
    rig.run(BEAT as usize); // Let the release finish.
    let open = settled(&mut rig);
    assert!(
        open > squeezed * 1.5,
        "a higher threshold compresses less: {squeezed} -> {open}"
    );
}

#[test]
fn bypass_and_reorder_keep_device_state() {
    use daw_devices::devices::{eq, utility};
    use daw_model::commands::{DeviceField, MoveDevice, SetDevice, SetParam};
    let (mut rig, ids) = effect_rig(&["daw.eq", "daw.utility"]);
    let (eq_id, util) = (ids[0], ids[1]);
    rig.doc
        .execute(SetParam {
            device: eq_id,
            param: eq::OUTPUT,
            value: Some(-6.0),
        })
        .unwrap();
    rig.doc
        .execute(SetParam {
            device: util,
            param: utility::GAIN,
            value: Some(6.0),
        })
        .unwrap();
    rig.sync();
    let both = settled(&mut rig);
    assert!((both - 0.5).abs() < 2e-3, "-6 then +6 dB: {both}");

    let set_enabled = |rig: &mut Rig, id, on| {
        rig.doc
            .execute(SetDevice {
                id,
                field: DeviceField::Enabled(on),
            })
            .unwrap();
        rig.sync();
    };
    set_enabled(&mut rig, eq_id, false);
    let bypassed = settled(&mut rig);
    assert!(
        (bypassed - 0.5 * 10f32.powf(6.0 / 20.0)).abs() < 2e-3,
        "only the utility: {bypassed}"
    );
    set_enabled(&mut rig, eq_id, true);
    // Moving keeps each device's settings: still -6 + 6 dB.
    rig.doc.execute(MoveDevice::new(eq_id, 1)).unwrap();
    rig.sync();
    let devices: Vec<_> = rig.doc.project().track(rig.track).unwrap().devices.clone();
    assert_eq!(devices[1].id, eq_id);
    let moved = settled(&mut rig);
    assert!((moved - 0.5).abs() < 2e-3, "{moved}");
}
