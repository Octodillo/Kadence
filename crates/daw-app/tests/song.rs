//! End-to-end "make a song" smoke test.
//!
//! Builds a short song the way the app does, with the app's own track,
//! instrument and mixer code and the undoable model commands its views
//! send: a project bundle, an imported audio clip on an audio track, a MIDI
//! clip with notes on an instrument track (which starts with the synth), a
//! tempo change, effect inserts (one of them a CLAP plugin running in its
//! sandbox host), a bus fed by a send, and volume automation. Then it checks
//! undo and redo, plays the song live, exports it to WAV and FLAC the way the
//! app's Export Song does, swaps the instrument to the sampler and back,
//! saves, reopens a moved copy, and checks that what comes out is
//! music-shaped: not silent, each part audible where it should be, and the
//! same after the round trip.
//!
//! No Qt runs here; it lives in this crate because the track, instrument
//! and mixer code it drives does. The plugin is `daw-clap-test-plugin`, a
//! gain, built with the sandbox host the first time the test needs them.
//!
//! CI runs this as its own step; set `KADENCE_SMOKE_OUT` to a directory to
//! keep the exported files.
//!
//! Features still on open pull requests have hooks marked `TODO(#n)`; extend
//! this test when they merge:
//! * #95 sends from the mixer: add the keys' send there.
//! * #98 device parameters: set the reverb and the plugin's gain through the
//!   device panel's commands, and check bypass.
//! * #103 plugin browser: add the plugin the way the browser does.
//! * #108 automation lanes: also automate a device parameter and a send
//!   level the way the arrange view writes them.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::Duration;

use assert_no_alloc::{AllocDisabler, assert_no_alloc, violation_count};
use daw_app::instrument::{self, InstrumentChoices};
use daw_app::mixer::{Control, StripId};
use daw_app::session_mixer::SessionMixer;
use daw_core::event::EventList;
use daw_core::plugin::PluginFormatKind;
use daw_core::time::{PPQ, SampleTime, TickTime};
use daw_engine::EngineConfig;
use daw_media::{AudioData, FileFormat, FlacBits, WavFormat, decode_file, write_wav};
use daw_model::bundle::to_json;
use daw_model::commands::{
    AddClip, AddDevice, AddLane, AddNotes, AddSend, ClipField, RemoveDevice, RemoveSend, SetClip,
    SetParam, SetTempo,
};
use daw_model::{
    AudioClip, AutomationLane, AutomationPoint, AutomationTarget, Clip, ClipContent, Device,
    DeviceId, DeviceSource, Document, Fade, FadeShape, MeterPoint, MidiClip, Note, Project, Send,
    SendId, TempoData, TempoPoint, Track, TrackId, TrackKind,
};
use daw_plugin_host::{HostLauncher, SandboxConfig};
use daw_session::export::{
    ExportJob, ExportSettings, LoaderFactory, Progress, RenderSettings, offline_sandbox, song_end,
};
use daw_session::{PluginStatus, SandboxLoader, Session, SessionConfig};

#[global_allocator]
static ALLOC: AllocDisabler = AllocDisabler;

const RATE: f64 = 48_000.0;
const BLOCK: usize = 256;
const BPM: f64 = 100.0;
/// Samples per beat at [`BPM`].
const BEAT: usize = (RATE * 60.0 / BPM) as usize;
/// Undo steps [`make_song`] makes.
const EDITS: usize = 16;
/// The song's clips end after three bars of 4/4.
const SONG_END_BEATS: usize = 12;
/// The export's tail, a fourth bar for the reverb to ring on.
const TAIL_BEATS: usize = 4;
/// The test plugin, a gain, and its gain parameter.
const PLUGIN_ID: &str = "org.daw.test.gain";
const GAIN: u32 = 1;
/// The latency the test plugin reports (`daw_clap_test_plugin::LATENCY`).
/// It doesn't actually delay, so a render that compensates for it plays the
/// plugin's track this much early.
const PLUGIN_LATENCY: usize = 32;

/// What the test needs to find again after building the song.
struct Song {
    doc: Document,
    keys: TrackId,
    keys_send: SendId,
    plugin: DeviceId,
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
    // After the EQ, the CLAP test plugin at half gain.
    // TODO(#103): add it the way the plugin browser does.
    let plugin = Device::new(
        "Test Gain",
        DeviceSource::Plugin {
            format: PluginFormatKind::Clap,
            id: PLUGIN_ID.into(),
        },
    );
    let plugin_id = plugin.id;
    doc.execute(AddDevice {
        track: pluck,
        index: 1,
        device: plugin,
    })
    .unwrap();
    doc.seal();
    // TODO(#98): set this through the device panel.
    doc.execute(SetParam {
        device: plugin_id,
        param: GAIN,
        value: Some(0.5),
    })
    .unwrap();
    doc.seal();
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
    // TODO(#95): add the send from the mixer.
    let send = Send::new(bus);
    let keys_send = send.id;
    doc.execute(AddSend { track: keys, send }).unwrap();
    doc.seal();

    // Volume automation: full level for the first chord, -12 dB from the
    // second on.
    // TODO(#108): draw this lane the way the arrange view does, and add a
    // device parameter and send level lane.
    let mut lane = AutomationLane::new(AutomationTarget::Volume);
    lane.points = vec![point(0.0, 0.0), point(8.0, -12.0)];
    doc.execute(AddLane { track: keys, lane }).unwrap();
    doc.seal();

    Song {
        doc,
        keys,
        keys_send,
        plugin: plugin_id,
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

/// Builds the test plugin and the sandbox host into their own target
/// directory (shared with daw-session's plugin tests) and returns a folder
/// holding only the plugin, and the host binary. The app is its own sandbox
/// host, which a test binary can't be.
fn plugin_binaries() -> &'static (PathBuf, PathBuf) {
    static PATHS: OnceLock<(PathBuf, PathBuf)> = OnceLock::new();
    PATHS.get_or_init(|| {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let target = root.join("target").join("clap-test-plugin");
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let status = Command::new(cargo)
            .args([
                "build",
                "--quiet",
                "--package",
                "daw-clap-test-plugin",
                "--package",
                "daw-plugin-host",
                "--bin",
                "daw-plugin-host",
                "--lib",
                "--target-dir",
            ])
            .arg(&target)
            .current_dir(&root)
            .status()
            .expect("run cargo");
        assert!(status.success(), "building the test plugin and host failed");
        // One folder for every run; the copy is renamed into place so a
        // plugin another run has loaded is never overwritten.
        let dir = target.join("song-plugins");
        std::fs::create_dir_all(&dir).unwrap();
        let lib = target.join("debug").join("libdaw_clap_test_plugin.so");
        let tmp = dir.join(format!(".test-gain-{}", std::process::id()));
        std::fs::copy(&lib, &tmp).expect("copy the test plugin");
        std::fs::rename(&tmp, dir.join("test-gain.clap")).unwrap();
        (dir, target.join("debug").join("daw-plugin-host"))
    })
}

/// A plugin loader that finds only the test plugin, set up for playback or,
/// with [`offline_sandbox`], for an export, as the app's engine sets up its
/// own.
fn plugin_loader(adjust: fn(SandboxConfig) -> SandboxConfig) -> SandboxLoader {
    let (plugins, host) = plugin_binaries();
    let mut config = SandboxConfig::new(HostLauncher::new(host, Vec::<String>::new()));
    // CI machines are slow and the host is a debug build. An export's
    // offline_sandbox replaces this with the hang timeout.
    config.process_timeout = Some(Duration::from_secs(5));
    config.rt_priority = None;
    let mut loader = SandboxLoader::new(adjust(config));
    let registry = loader.registry_mut();
    registry.set_search_paths(PluginFormatKind::Clap, vec![plugins.clone()]);
    registry.set_search_paths(PluginFormatKind::Lv2, vec![]);
    registry.set_search_paths(PluginFormatKind::Vst3, vec![]);
    loader
}

/// The whole song, from the top to the end of its last clip as Export
/// Song's default range, plus a bar of tail (the dialog's default is 2 s).
fn render_settings(doc: &Document) -> RenderSettings {
    let end = song_end(doc.project());
    assert_eq!(
        end,
        beats(SONG_END_BEATS as f64),
        "the song ends with its last clip"
    );
    RenderSettings {
        start: TickTime(0),
        end,
        tail_seconds: (TAIL_BEATS * BEAT) as f64 / RATE,
        sample_rate: RATE as u32,
    }
}

/// Renders the song offline through the export's render, with plugins.
fn render(doc: &mut Document) -> AudioData {
    let loader: Box<dyn daw_session::PluginLoader> = Box::new(plugin_loader(offline_sandbox));
    let mut channels = vec![Vec::new(), Vec::new()];
    let warnings = daw_session::export::render(
        &doc.snapshot(),
        doc.bundle().map(|b| b.root().to_path_buf()),
        Some(loader),
        &render_settings(doc),
        &Progress::default(),
        |planes| {
            for (c, p) in channels.iter_mut().zip(planes) {
                c.extend_from_slice(p);
            }
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(warnings, [], "the song renders without warnings");
    assert_eq!(channels[0].len(), (SONG_END_BEATS + TAIL_BEATS) * BEAT);
    AudioData {
        sample_rate: RATE as u32,
        channels,
    }
}

/// Exports the song to `path` the way the app's Export Song does: a snapshot of
/// the saved project, rendered on the export's own thread with a plugin
/// loader made there.
fn export(doc: &mut Document, path: PathBuf, format: FileFormat) {
    let factory: LoaderFactory = Box::new(|| Some(Box::new(plugin_loader(offline_sandbox))));
    let job = ExportJob::start(
        doc.snapshot(),
        doc.bundle().map(|b| b.root().to_path_buf()),
        Some(factory),
        path,
        ExportSettings {
            render: render_settings(doc),
            format,
            dither: false,
        },
    )
    .unwrap();
    let report = job.wait().unwrap();
    assert_eq!(report.warnings, [], "the song exports without warnings");
    assert_eq!(report.frames, ((SONG_END_BEATS + TAIL_BEATS) * BEAT) as u64);
}

/// Plays the first `n` beats live, the way the audio thread does, ticking
/// the session as the app's timer would, and checks the plugin runs and
/// nothing allocates. Then stores the plugins' states in the document, as
/// the app does before saving.
fn play(doc: &mut Document, plugin: DeviceId, n: usize) -> AudioData {
    let (mut session, mut proc) = Session::new(config());
    session.set_root(doc.bundle().map(|b| b.root().to_path_buf()));
    session.set_plugin_loader(Some(Box::new(plugin_loader(|c| c))));
    session.sync(&doc.snapshot()).unwrap();
    assert_eq!(session.warnings(), &[], "the song plays without warnings");
    assert_eq!(session.plugin_status(plugin), Some(PluginStatus::Running));
    session.locate(TickTime(0)).unwrap();
    session.play().unwrap();

    let frames = n * BEAT;
    let mut channels = vec![Vec::with_capacity(frames), Vec::with_capacity(frames)];
    let mut l = vec![0.0; BLOCK];
    let mut r = vec![0.0; BLOCK];
    let events = EventList::with_capacity(0);
    while channels[0].len() < frames {
        let n = BLOCK.min(frames - channels[0].len());
        let mut outs: [&mut [f32]; 2] = [&mut l[..n], &mut r[..n]];
        let before = violation_count();
        assert_no_alloc(|| proc.process(n, &[], &mut outs, &events));
        assert_eq!(violation_count(), before, "process allocated");
        channels[0].extend_from_slice(&l[..n]);
        channels[1].extend_from_slice(&r[..n]);
        let report = session.tick();
        assert!(report.error.is_none(), "{:?}", report.error);
        assert_eq!(report.warnings, [], "the song plays without warnings");
    }
    let states = session.plugin_states();
    assert_eq!(states.len(), 1, "the plugin has a state to save");
    for s in states {
        assert_eq!(s.device, plugin);
        let state = s.state.unwrap();
        // The test plugin's state is its gain.
        let mut gain = b"DAWT".to_vec();
        gain.extend_from_slice(&0.5f64.to_le_bytes());
        assert_eq!(&*state.0, &gain[..], "the plugin took the gain parameter");
        doc.store_plugin_state(s.device, Some(state)).unwrap();
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
        plugin,
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

    // Play the first bar live, which also stores the plugin's state, and
    // save.
    let live = play(&mut doc, plugin, SONG_END_BEATS + TAIL_BEATS);
    doc.save().unwrap();

    // Export to WAV and FLAC, as Export Song does with the saved project.
    // Written before checking them, so a failing run leaves them to listen
    // to.
    let wav = bundle.join("Smoke Song.wav");
    let flac = bundle.join("Smoke Song.flac");
    let keep = |file: &Path, name: &str| {
        if let Some(out) = std::env::var_os("KADENCE_SMOKE_OUT") {
            let out = Path::new(&out);
            std::fs::create_dir_all(out).unwrap();
            std::fs::copy(file, out.join(name)).unwrap();
        }
    };
    export(&mut doc, wav.clone(), FileFormat::Wav(WavFormat::Float32));
    keep(&wav, "smoke-song.wav");
    export(&mut doc, flac.clone(), FileFormat::Flac(FlacBits::B24));
    keep(&flac, "smoke-song.flac");

    // The WAV holds the render, exactly; the FLAC to within its 24 bits.
    let song = render(&mut doc);
    let read = decode_file(&wav).unwrap();
    assert_eq!(read.sample_rate, RATE as u32);
    assert_eq!(read.channels, song.channels, "the WAV holds the render");
    let read = decode_file(&flac).unwrap();
    assert_eq!(read.sample_rate, RATE as u32);
    assert_eq!(read.channels.len(), 2, "the FLAC is stereo");
    assert_eq!(read.channels[0].len(), song.channels[0].len());
    let error = read
        .channels
        .iter()
        .flatten()
        .zip(song.channels.iter().flatten())
        .fold(0f32, |m, (a, b)| m.max((a - b.clamp(-1.0, 1.0)).abs()));
    assert!(
        error < 2.0 / 8_388_608.0,
        "the FLAC holds the render ({error})"
    );

    // Check each part of the song is where it should be.
    let all = song.channels.iter().flatten();
    assert!(all.clone().all(|x| x.is_finite()), "no NaN or infinity");
    let peak = all.fold(0f32, |m, x| m.max(x.abs()));
    assert!(peak > 0.05, "the song is not silent (peak {peak})");
    assert!(peak < 2.0, "the song does not blow up (peak {peak})");

    let pluck = rms(&song, 0.1, 3.0);
    assert!(pluck > 0.01, "the audio clip plays in bar 1 ({pluck})");
    // Live playback is the export, delayed by the plugin's latency (which
    // the export leaves out), sample for sample give or take rounding in
    // the different block sizes.
    let lag = live
        .channels
        .iter()
        .zip(&song.channels)
        .flat_map(|(l, s)| l[PLUGIN_LATENCY..].iter().zip(s))
        .fold(0f32, |m, (l, s)| m.max((l - s).abs()));
    assert!(lag < 1e-5, "playback and export agree (off by {lag})");
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

    // The plugin halves the pluck: without it, bar 1 is twice as loud.
    doc.execute(RemoveDevice::new(plugin)).unwrap();
    let unplugged = render(&mut doc);
    doc.undo().unwrap();
    // Without it there is no latency to compensate either: the export
    // starts at the top of the song with or without plugins.
    let off = (0..2)
        .flat_map(|c| {
            let (u, s) = (&unplugged.channels[c], &song.channels[c]);
            (0..4 * BEAT - PLUGIN_LATENCY).map(move |i| (u[i + PLUGIN_LATENCY] - 2.0 * s[i]).abs())
        })
        .fold(0f32, f32::max);
    assert!(
        off < 1e-6,
        "the plugin plays in the export at half gain, and the export skips its latency (off by {off})"
    );
    assert!(
        unplugged.channels[0][..64].iter().any(|&x| x != 0.0),
        "the export starts at the top of the song"
    );

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
