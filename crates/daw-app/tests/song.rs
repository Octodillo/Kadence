//! End-to-end "make a song" smoke test.
//!
//! Builds a short song the way the app does, with the app's own track,
//! instrument and mixer code and the undoable model commands its views
//! send: a project bundle, an imported audio clip on an audio track, a MIDI
//! clip with notes on an instrument track (which starts with the synth), a
//! tempo change, effect inserts, a bus fed by a send, and volume
//! automation. Then it checks undo and redo, renders the song offline,
//! swaps the instrument to the sampler and back, saves, reopens a moved
//! copy, and checks that what comes out is music-shaped: not silent, each
//! part audible where it should be, and the same after the round trip.
//!
//! No Qt runs here; it lives in this crate because the track, instrument
//! and mixer code it drives does.
//!
//! CI runs this as its own step; set `KADENCE_SMOKE_OUT` to a directory to
//! keep the rendered WAV.
//!
//! Features still on open pull requests have hooks marked `TODO(#n)`; extend
//! this test when they merge:
//! * #98 device parameters: set the reverb through the device panel's
//!   commands, and check bypass.
//! * #91 plugin devices: add the CLAP test plugin as an insert and check it
//!   is loaded (no `PluginNotLoaded` warning) and audible.
//! * #96 export: replace [`render`] and the WAV writing with the app's export
//!   to WAV and FLAC.
//! * #108 automation lanes: also automate a device parameter and a send
//!   level the way the arrange view writes them.

use std::path::{Path, PathBuf};

use assert_no_alloc::{AllocDisabler, assert_no_alloc, violation_count};
use daw_app::instrument::{self, InstrumentChoices};
use daw_app::mixer::{Control, StripId};
use daw_app::session_mixer::SessionMixer;
use daw_core::event::EventList;
use daw_core::time::{PPQ, SampleTime, TickTime};
use daw_engine::EngineConfig;
use daw_media::{AudioData, WavFormat, decode_file, write_wav};
use daw_model::bundle::to_json;
use daw_model::commands::{
    AddClip, AddLane, AddNotes, ClipField, RemoveSend, SetClip, SetParam, SetTempo,
};
use daw_model::{
    AudioClip, AutomationLane, AutomationPoint, AutomationTarget, Clip, ClipContent, DeviceSource,
    Document, Fade, FadeShape, MeterPoint, MidiClip, Note, Project, SendId, TempoData, TempoPoint,
    Track, TrackId, TrackKind,
};
use daw_session::{Session, SessionConfig};

#[global_allocator]
static ALLOC: AllocDisabler = AllocDisabler;

const RATE: f64 = 48_000.0;
const BLOCK: usize = 256;
const BPM: f64 = 100.0;
/// Samples per beat at [`BPM`].
const BEAT: usize = (RATE * 60.0 / BPM) as usize;
/// Undo steps [`make_song`] makes.
const EDITS: usize = 14;
/// The song is four bars of 4/4.
const SONG_BEATS: usize = 16;

/// What the test needs to find again after building the song.
struct Song {
    doc: Document,
    keys: TrackId,
    keys_send: SendId,
    instruments: InstrumentChoices,
    mixer: SessionMixer,
    /// The project file of the new, empty project.
    empty: String,
}

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

fn beats(n: f64) -> TickTime {
    TickTime((n * PPQ as f64).round() as i64)
}

/// A one-bar stereo pluck at 110 Hz, as if recorded somewhere else, to
/// import into the bundle.
fn pluck_file(dir: &Path) -> PathBuf {
    let frames = 4 * BEAT;
    let l: Vec<f32> = (0..frames)
        .map(|i| {
            let t = (i % BEAT) as f32 / RATE as f32;
            let x = std::f32::consts::TAU * 110.0 * i as f32 / RATE as f32;
            x.sin() * (-t * 5.0).exp() * 0.5
        })
        .collect();
    let r = l.iter().map(|x| x * 0.9).collect();
    let path = dir.join("pluck.wav");
    let data = AudioData {
        sample_rate: RATE as u32,
        channels: vec![l, r],
    };
    write_wav(&path, &data, WavFormat::Float32).unwrap();
    path
}

/// The mixer strip of a track.
fn strip(track: TrackId) -> StripId {
    track.0.as_u128()
}

/// Makes the song in a new bundle at `bundle`, one edit at a time, through
/// the same code the app's track menu, instrument menu and mixer run. Each
/// edit is sealed as its own undo step, as the app does after a gesture.
///
/// Bar 1: the audio track's pluck. Bars 2 and 3: the same chord twice on
/// the instrument track, the second 12 dB quieter through volume
/// automation. Bar 4: only the reverb tail the keys send to the bus.
fn make_song(bundle: &Path, source: &Path) -> Song {
    let mut doc = Document::new(Project::new("Smoke Song"));
    doc.save_as(bundle).unwrap();
    let empty = to_json(doc.project()).unwrap();
    let instruments = InstrumentChoices::builtin();
    let mut mixer = SessionMixer::default();
    let add_track = |doc: &mut Document, at: usize, kind: TrackKind, name: &str| {
        let track = Track::new(kind, name);
        let id = track.id;
        doc.execute_boxed(instrument::add_track(
            at,
            track,
            instruments.default_choice(),
        ))
        .unwrap();
        doc.seal();
        id
    };

    doc.execute(SetTempo {
        tempo: TempoData {
            tempos: vec![TempoPoint {
                tick: TickTime(0),
                bpm: BPM,
                ramp: false,
            }],
            meters: vec![MeterPoint {
                tick: TickTime(0),
                numerator: 4,
                denominator: 4,
            }],
        },
    })
    .unwrap();
    doc.seal();

    // An audio track with an imported clip, an EQ insert, a little pan and
    // a fade out.
    let pluck = add_track(&mut doc, 0, TrackKind::Audio, "Pluck");
    let stored = doc.bundle().unwrap().import_audio(source).unwrap();
    assert!(stored.is_relative(), "{}", stored.display());
    let clip = Clip::new(
        "Pluck",
        TickTime(0),
        beats(4.0),
        ClipContent::Audio(AudioClip::new(stored, SampleTime(0))),
    );
    let clip_id = clip.id;
    doc.execute(AddClip::new(pluck, clip)).unwrap();
    doc.seal();
    doc.execute(SetClip::new(
        clip_id,
        ClipField::FadeOut(Fade::new(beats(1.0), FadeShape::EqualPower)),
    ))
    .unwrap();
    doc.seal();
    assert!(mixer.add_insert(&mut doc, strip(pluck), "daw.eq"));
    mixer
        .set(&mut doc, None, strip(pluck), Control::Pan, -0.2)
        .unwrap();

    // An instrument track, which starts with the synth, playing a MIDI clip.
    let keys = add_track(&mut doc, 1, TrackKind::Instrument, "Keys");
    doc.seal();
    let t = doc.project().track(keys).unwrap();
    assert_eq!(
        instrument::instrument_of(t).map(|d| &d.source),
        Some(&builtin_source(instrument::DEFAULT_INSTRUMENT)),
        "a new instrument track has the synth"
    );
    let midi = Clip::new(
        "Chords",
        beats(4.0),
        beats(8.0),
        ClipContent::Midi(MidiClip::default()),
    );
    let midi_id = midi.id;
    doc.execute(AddClip::new(keys, midi)).unwrap();
    doc.seal();
    // The same C major chord in each bar, so the two can be compared.
    let notes = [0.0, 4.0]
        .into_iter()
        .flat_map(|start| [60, 64, 67].map(|key| Note::new(beats(start), beats(3.5), key, 0.8)))
        .collect();
    doc.execute(AddNotes {
        clip: midi_id,
        notes,
    })
    .unwrap();
    doc.seal();

    // A bus with a fully wet reverb, fed by a send from the keys.
    let bus = add_track(&mut doc, 2, TrackKind::Bus, "Reverb");
    assert!(mixer.add_insert(&mut doc, strip(bus), "daw.reverb"));
    let reverb = doc.project().track(bus).unwrap().devices[0].id;
    // TODO(#98): set this through the device panel.
    doc.execute(SetParam {
        device: reverb,
        param: daw_devices::devices::reverb::MIX,
        value: Some(1.0),
    })
    .unwrap();
    doc.seal();
    // The keys' send into the reverb bus, added from the keys' strip.
    let choices = mixer
        .strips(doc.project(), None)
        .into_iter()
        .find(|s| s.id == strip(keys))
        .unwrap()
        .send_choices;
    assert!(choices.iter().any(|c| c.id == strip(bus)), "{choices:?}");
    assert!(mixer.add_send(&mut doc, strip(keys), strip(bus)));
    let keys_send = doc.project().track(keys).unwrap().sends[0].id;

    // Volume automation: full level for the first chord, -12 dB from the
    // second on.
    // TODO(#108): draw this lane the way the arrange view does, and add a
    // device parameter and send level lane.
    let mut lane = AutomationLane::new(AutomationTarget::Volume);
    lane.points = vec![point(0.0, 0.0), point(8.0, -12.0)];
    doc.execute(AddLane { track: keys, lane }).unwrap();
    doc.seal();

    // TODO(#91): add the CLAP test plugin as an insert.
    Song {
        doc,
        keys,
        keys_send,
        empty,
        instruments,
        mixer,
    }
}

fn builtin_source(kind: &str) -> DeviceSource {
    DeviceSource::Builtin { kind: kind.into() }
}

fn point(beat: f64, db: f64) -> AutomationPoint {
    AutomationPoint {
        tick: beats(beat),
        value: db,
        curve: 0.0,
        step: true,
    }
}

/// Renders the whole song offline from the start, as stereo.
// TODO(#96): use the app's export instead.
fn render(doc: &mut Document) -> AudioData {
    let (mut session, mut proc) = Session::new(config());
    session.set_root(doc.bundle().map(|b| b.root().to_path_buf()));
    session.sync(&doc.snapshot()).unwrap();
    assert_eq!(session.warnings(), &[], "the song plays without warnings");
    session.locate(TickTime(0)).unwrap();
    session.play().unwrap();

    let frames = SONG_BEATS * BEAT;
    let mut channels = vec![Vec::with_capacity(frames), Vec::with_capacity(frames)];
    let mut l = vec![0.0; BLOCK];
    let mut r = vec![0.0; BLOCK];
    let events = EventList::with_capacity(0);
    let mut done = 0;
    while done < frames {
        let n = BLOCK.min(frames - done);
        let mut outs: [&mut [f32]; 2] = [&mut l[..n], &mut r[..n]];
        let before = violation_count();
        assert_no_alloc(|| proc.process(n, &[], &mut outs, &events));
        assert_eq!(violation_count(), before, "process allocated");
        channels[0].extend_from_slice(&l[..n]);
        channels[1].extend_from_slice(&r[..n]);
        done += n;
    }
    AudioData {
        sample_rate: RATE as u32,
        channels,
    }
}

/// RMS of both channels between two beat positions.
fn rms(data: &AudioData, from: f64, to: f64) -> f64 {
    let (a, b) = ((from * BEAT as f64) as usize, (to * BEAT as f64) as usize);
    let sum: f64 = data
        .channels
        .iter()
        .flat_map(|c| &c[a..b])
        .map(|&x| f64::from(x) * f64::from(x))
        .sum();
    (sum / (2 * (b - a)) as f64).sqrt()
}

#[test]
fn make_save_reload_and_render_a_song() {
    let tmp = tempfile::tempdir().unwrap();
    let source = pluck_file(tmp.path());
    let bundle = tmp.path().join("Smoke Song");
    let Song {
        mut doc,
        keys,
        keys_send,
        empty,
        instruments,
        mut mixer,
    } = make_song(&bundle, &source);

    // Undo everything and redo it: the document comes back the same.
    let made = to_json(doc.project()).unwrap();
    let mut undone = 0;
    while doc.undo().unwrap().is_some() {
        undone += 1;
    }
    assert_eq!(undone, EDITS, "every edit is its own undo step");
    assert_eq!(
        to_json(doc.project()).unwrap(),
        empty,
        "undo returns to the empty project"
    );
    while doc.redo().unwrap().is_some() {}
    assert_eq!(to_json(doc.project()).unwrap(), made, "redo restores it");

    // Render and check each part of the song is where it should be.
    doc.save().unwrap();
    let song = render(&mut doc);
    // Written before checking it, so a failing run leaves it to listen to.
    // TODO(#96): export through the app's export (WAV and FLAC).
    let wav = bundle.join("Smoke Song.wav");
    write_wav(&wav, &song, WavFormat::Float32).unwrap();
    if let Some(out) = std::env::var_os("KADENCE_SMOKE_OUT") {
        std::fs::create_dir_all(&out).unwrap();
        std::fs::copy(&wav, Path::new(&out).join("smoke-song.wav")).unwrap();
    }
    let all = song.channels.iter().flatten();
    assert!(all.clone().all(|x| x.is_finite()), "no NaN or infinity");
    let peak = all.fold(0f32, |m, x| m.max(x.abs()));
    assert!(peak > 0.05, "the song is not silent (peak {peak})");
    assert!(peak < 2.0, "the song does not blow up (peak {peak})");

    let pluck = rms(&song, 0.1, 3.0);
    assert!(pluck > 0.01, "the audio clip plays in bar 1 ({pluck})");
    assert!(
        song.channels[0][1..64].iter().any(|&x| x != 0.0),
        "the render starts at the top of the song"
    );
    // The source repeats every beat, so the end of its last beat would
    // match the end of its first without the fade out.
    let (first, last) = (rms(&song, 0.5, 1.0), rms(&song, 3.5, 4.0));
    assert!(last < 0.6 * first, "the clip fades out ({last} vs {first})");
    // The source's right channel is 0.9 of its left; panning left lowers
    // it further.
    let side = |c: usize| {
        let ch = &song.channels[c][..3 * BEAT];
        ch.iter().map(|&x| f64::from(x).powi(2)).sum::<f64>().sqrt()
    };
    let balance = side(1) / side(0);
    assert!(
        balance < 0.85,
        "the pan moves the pluck left (R/L {balance})"
    );
    let loud = rms(&song, 4.5, 7.0);
    let quiet = rms(&song, 8.5, 11.0);
    assert!(loud > 0.01, "the MIDI clip plays the synth ({loud})");
    let ratio = quiet / loud;
    // -12 dB is 0.25; the reverb tail of the first chord adds a little.
    assert!(
        (0.2..0.32).contains(&ratio),
        "volume automation turns the second chord down ({quiet} / {loud} = {ratio})"
    );
    let tail = rms(&song, 12.5, 15.5);
    eprintln!("pluck {pluck:.4}, chord {loud:.4} then {quiet:.4}, reverb tail {tail:.5}");

    // The tail in bar 4 comes from the send into the reverb bus: without
    // the send it (nearly) goes away.
    doc.execute(RemoveSend::new(keys_send)).unwrap();
    let dry = render(&mut doc);
    doc.undo().unwrap();
    let dry_tail = rms(&dry, 12.5, 15.5);
    assert!(
        // The reverb's default 30% mix leaves about a third of this.
        tail > 5e-4 && tail > 4.0 * dry_tail,
        "the send feeds the reverb bus (tail {tail}, without the send {dry_tail})"
    );
    assert!(!doc.is_modified(), "undo returns to the saved state");

    // Swap the synth for the sampler from the mixer. With no samples
    // loaded the keys go quiet, nothing else changes, and undo brings the
    // synth back. Silence alone can't tell an empty sampler from a dropped
    // instrument; the model check and the no-warnings check in `render`
    // cover that until the sampler can be given a sample here.
    let sampler = instruments
        .position(&builtin_source("daw.sampler"))
        .and_then(|i| instruments.get(i))
        .unwrap();
    mixer
        .set_instrument(&mut doc, strip(keys), sampler)
        .unwrap();
    let t = doc.project().track(keys).unwrap();
    assert_eq!(t.devices.len(), 1, "the instrument is replaced, not added");
    assert_eq!(t.devices[0].source, builtin_source("daw.sampler"));
    let swapped = render(&mut doc);
    let keys_part = rms(&swapped, 4.5, 15.5);
    assert!(keys_part < 1e-4, "an empty sampler is silent ({keys_part})");
    // Up to the MIDI clip at beat 4 nothing changes; the synth sounds
    // right from there.
    let at = |beat: f64| (beat * BEAT as f64) as usize;
    for c in 0..2 {
        assert_eq!(
            swapped.channels[c][..at(4.0)],
            song.channels[c][..at(4.0)],
            "the other tracks play on"
        );
        assert_ne!(
            swapped.channels[c][at(4.0)..at(4.05)],
            song.channels[c][at(4.0)..at(4.05)],
            "the keys start at beat 4"
        );
    }
    doc.undo().unwrap();
    assert!(!doc.is_modified(), "one undo puts the synth back");
    assert_eq!(render(&mut doc).channels, song.channels);

    // The exported file holds the render.
    let read = decode_file(&wav).unwrap();
    assert_eq!(read.sample_rate, RATE as u32);
    assert_eq!(read.channels, song.channels, "the WAV holds the render");

    // Reopen from disk, from a copy of the bundle somewhere else, so
    // nothing depends on absolute paths.
    let moved = tmp.path().join("elsewhere").join("Smoke Song");
    copy_dir(&bundle, &moved);
    std::fs::remove_dir_all(&bundle).unwrap();
    std::fs::remove_file(&source).unwrap();
    let mut reopened = Document::open(&moved).unwrap();
    assert_eq!(reopened.project(), doc.project(), "the project round-trips");
    assert!(!reopened.is_modified());

    // Saving again writes the same file.
    let file = reopened.bundle().unwrap().project_file();
    let first = std::fs::read_to_string(&file).unwrap();
    reopened.save().unwrap();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), first);

    // And it sounds the same.
    let again = render(&mut reopened);
    assert_eq!(
        again.channels, song.channels,
        "the reloaded song renders the same"
    );
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let dest = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &dest);
        } else {
            std::fs::copy(entry.path(), dest).unwrap();
        }
    }
}
