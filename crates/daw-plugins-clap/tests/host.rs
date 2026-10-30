//! Loads the `daw-clap-test-plugin` CLAP from disk and drives it through the contract traits.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use assert_no_alloc::{AllocDisabler, assert_no_alloc};
use daw_core::event::{Event, EventKind, EventList};
use daw_core::node::{ProcessContext, TransportState};
use daw_core::plugin::{
    PluginDescriptor, PluginError, PluginFormat, PluginFormatKind, PluginInstance,
};
use daw_plugins_clap::ClapFormat;

#[global_allocator]
static ALLOC: AllocDisabler = AllocDisabler;

const ID: &str = "org.daw.test.gain";
const GAIN: u32 = 1;
const LAST_NOTE: u32 = 2;
const SR: f64 = 48_000.0;
const MAX: u32 = 64;

/// Builds the test plugin in its own target directory (so it never waits on the outer build's
/// lock) and returns the path of a `.clap` copy of it. The copy is made once per process:
/// overwriting a library that is already loaded corrupts its mapping.
fn test_plugin() -> PathBuf {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(build_test_plugin).clone()
}

fn build_test_plugin() -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let target = root.join("target").join("clap-test-plugin");
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let status = Command::new(cargo)
        .args([
            "build",
            "--quiet",
            "--package",
            "daw-clap-test-plugin",
            "--target-dir",
        ])
        .arg(&target)
        .current_dir(&root)
        .status()
        .expect("run cargo");
    assert!(status.success(), "building the test plugin failed");
    let lib = target.join("debug").join("libdaw_clap_test_plugin.so");
    let clap = target.join(format!("daw-test-{}.clap", std::process::id()));
    std::fs::copy(&lib, &clap).expect("copy the test plugin");
    clap
}

fn descriptor() -> PluginDescriptor {
    let path = test_plugin();
    let mut found = ClapFormat.scan(&path).expect("the test plugin loads");
    assert_eq!(found.len(), 1, "one plugin in the test bundle");
    found.remove(0)
}

fn instance() -> Box<dyn PluginInstance> {
    ClapFormat
        .instantiate(&descriptor(), SR, MAX)
        .expect("instantiate")
}

/// Runs one block of `frames` stereo frames with every input sample set to `level`.
fn run(
    plugin: &mut dyn PluginInstance,
    frames: usize,
    level: f32,
    events: &[Event],
) -> ([Vec<f32>; 2], Vec<Event>) {
    run_with(&mut |ctx| plugin.process(ctx), frames, level, events)
}

/// [`run`] on whatever `process` calls, such as a split-off audio half.
fn run_with(
    process: &mut dyn FnMut(&mut ProcessContext),
    frames: usize,
    level: f32,
    events: &[Event],
) -> ([Vec<f32>; 2], Vec<Event>) {
    let input = vec![level; frames];
    let mut left = vec![0.0; frames];
    let mut right = vec![0.0; frames];
    let mut events_in = EventList::with_capacity(64);
    for e in events {
        events_in.push(*e).unwrap();
    }
    let mut events_out = EventList::with_capacity(64);
    let transport = TransportState {
        playing: true,
        bpm: 120.0,
        sample_rate: SR,
        ..TransportState::default()
    };
    {
        let audio_in = [&input[..], &input[..]];
        let mut audio_out = [&mut left[..], &mut right[..]];
        let mut ctx = ProcessContext {
            frames: u32::try_from(frames).unwrap(),
            transport: &transport,
            audio_in: &audio_in,
            audio_out: &mut audio_out,
            events_in: &events_in,
            events_out: &mut events_out,
        };
        process(&mut ctx);
    }
    let out = events_out.iter().copied().collect();
    ([left, right], out)
}

fn param(param: u32, value: f64, frame: u32) -> Event {
    Event {
        frame,
        kind: EventKind::ParamValue { param, value },
    }
}

#[test]
fn scan_describes_the_plugin() {
    let d = descriptor();
    assert_eq!(d.format, PluginFormatKind::Clap);
    assert_eq!(d.id, ID);
    assert_eq!(d.name, "DAW Test Gain");
    assert_eq!(d.vendor, "DAW");
    assert_eq!(d.ports, (2, 2));
    assert!(!d.is_instrument);
}

/// A file that does not load is a failure that says why, not a plugin-less bundle.
#[test]
fn scanning_a_non_plugin_reports_why() {
    let path = std::env::temp_dir().join(format!("not-a-plugin-{}.clap", std::process::id()));
    std::fs::write(&path, b"nope").unwrap();
    let result = ClapFormat.scan(&path);
    std::fs::remove_file(&path).unwrap();
    match result {
        Err(PluginError::Load(msg)) => assert!(msg.contains("not-a-plugin"), "{msg}"),
        other => panic!("expected a load error, got {other:?}"),
    }
}

#[test]
fn reports_ports_params_and_latency() {
    let plugin = instance();
    let ports = plugin.ports();
    assert_eq!((ports.audio_in, ports.audio_out), (2, 2));
    assert!(ports.event_in && ports.event_out);
    assert_eq!(plugin.latency(), 32);

    let params = plugin.params();
    assert_eq!(params.len(), 2);
    let gain = params.iter().find(|p| p.id == GAIN).unwrap();
    assert_eq!(gain.name, "Gain");
    assert_eq!((gain.min, gain.max, gain.default), (0.0, 2.0, 1.0));
    assert!(gain.automatable);
    let last = params.iter().find(|p| p.id == LAST_NOTE).unwrap();
    assert!(!last.automatable, "read-only params are not automatable");
    assert!(!plugin.has_editor());
}

#[test]
fn processes_audio_in_blocks_larger_than_max_frames() {
    let mut plugin = instance();
    let ([l, r], _) = run(plugin.as_mut(), 300, 0.5, &[]);
    assert!(l.iter().chain(&r).all(|&s| s == 0.5));
}

#[test]
fn param_events_are_sample_accurate() {
    let mut plugin = instance();
    // Frame 100 is inside the second internal 64-frame chunk.
    let ([l, r], _) = run(plugin.as_mut(), 200, 0.5, &[param(GAIN, 0.5, 100)]);
    assert!(l[..100].iter().all(|&s| s == 0.5));
    assert!(l[100..].iter().all(|&s| s == 0.25));
    assert_eq!(l, r);
    assert_eq!(plugin.param_value(GAIN), 0.5);
}

#[test]
fn note_events_round_trip_and_params_come_back_out() {
    let mut plugin = instance();
    let on = Event {
        frame: 70,
        kind: EventKind::NoteOn {
            channel: 1,
            key: 64,
            velocity: 0.5,
            note_id: 7,
        },
    };
    let midi = Event {
        frame: 10,
        kind: EventKind::Midi([0xb0, 1, 99]),
    };
    let (_, out) = run(plugin.as_mut(), 128, 0.0, &[midi, on]);
    assert!(out.contains(&midi), "MIDI echoed: {out:?}");
    assert!(
        out.contains(&on),
        "note on echoed at the same frame: {out:?}"
    );
    assert!(out.contains(&param(LAST_NOTE, 64.0, 70)), "{out:?}");
    assert_eq!(plugin.param_value(LAST_NOTE), 64.0);
}

#[test]
fn state_round_trips_into_a_new_instance() {
    let mut a = instance();
    run(a.as_mut(), 16, 0.0, &[param(GAIN, 0.25, 0)]);
    let state = a.save_state().unwrap();

    let mut b = instance();
    assert_eq!(b.param_value(GAIN), 1.0);
    b.load_state(&state).unwrap();
    assert_eq!(b.param_value(GAIN), 0.25);
    let ([l, _], _) = run(b.as_mut(), 16, 1.0, &[]);
    assert!(l.iter().all(|&s| s == 0.25));

    assert!(b.load_state(b"garbage").is_err());
}

#[test]
fn process_does_not_allocate() {
    let mut plugin = instance();
    let frames = 256;
    let input = vec![0.5; frames];
    let mut left = vec![0.0; frames];
    let mut right = vec![0.0; frames];
    let mut events_in = EventList::with_capacity(8);
    events_in.push(param(GAIN, 0.5, 3)).unwrap();
    events_in
        .push(Event {
            frame: 90,
            kind: EventKind::NoteOn {
                channel: 0,
                key: 60,
                velocity: 1.0,
                note_id: -1,
            },
        })
        .unwrap();
    let mut events_out = EventList::with_capacity(8);
    let transport = TransportState::default();
    for _ in 0..3 {
        events_out.clear();
        let audio_in = [&input[..], &input[..]];
        let mut audio_out = [&mut left[..], &mut right[..]];
        let mut ctx = ProcessContext {
            frames: 256,
            transport: &transport,
            audio_in: &audio_in,
            audio_out: &mut audio_out,
            events_in: &events_in,
            events_out: &mut events_out,
        };
        assert_no_alloc(|| plugin.process(&mut ctx));
    }
    // The gain change from the first block persists into later ones.
    assert_eq!(left[0], 0.25);
    assert_eq!(left[255], 0.25);
    assert_no_alloc(|| plugin.reset());
}

#[test]
fn prepare_again_reactivates() {
    let mut plugin = instance();
    run(plugin.as_mut(), 16, 0.0, &[param(GAIN, 2.0, 0)]);
    plugin.prepare(44_100.0, 512);
    plugin.on_main_thread();
    let ([l, _], _) = run(plugin.as_mut(), 1000, 0.25, &[]);
    assert!(l.iter().all(|&s| s == 0.5));
}

#[test]
fn instantiate_rejects_bad_descriptors() {
    let mut d = descriptor();
    d.format = PluginFormatKind::Lv2;
    assert!(ClapFormat.instantiate(&d, SR, MAX).is_err());
    let mut d = descriptor();
    d.id = "org.daw.missing".into();
    assert!(ClapFormat.instantiate(&d, SR, MAX).is_err());
    d.path = PathBuf::from("/nonexistent/plugin.clap");
    assert!(ClapFormat.instantiate(&d, SR, MAX).is_err());
}

#[test]
fn a_flood_of_output_events_is_dropped_without_allocating() {
    let mut plugin = instance();
    let frames = 64;
    let input = vec![0.0; frames];
    let mut left = vec![0.0; frames];
    let mut right = vec![0.0; frames];
    let mut events_in = EventList::with_capacity(100);
    for key in 0..100u8 {
        events_in
            .push(Event {
                frame: u32::from(key % 64),
                kind: EventKind::NoteOn {
                    channel: 0,
                    key,
                    velocity: 1.0,
                    note_id: -1,
                },
            })
            .unwrap();
    }
    // Each note on comes back as a note on and a param event: 200 events into room for 8.
    let mut events_out = EventList::with_capacity(8);
    let transport = TransportState::default();
    let audio_in = [&input[..], &input[..]];
    let mut audio_out = [&mut left[..], &mut right[..]];
    let mut ctx = ProcessContext {
        frames: 64,
        transport: &transport,
        audio_in: &audio_in,
        audio_out: &mut audio_out,
        events_in: &events_in,
        events_out: &mut events_out,
    };
    plugin.process(&mut ctx); // warm up: starts processing
    ctx.events_out.clear();
    assert_no_alloc(|| plugin.process(&mut ctx));
    assert_eq!(ctx.events_out.len(), 8);
}

#[test]
fn only_the_main_ports_reach_the_graph() {
    // The test plugin also has a sidechain input and a constant-1.0 aux output.
    let mut plugin = instance();
    let ports = plugin.ports();
    assert_eq!((ports.audio_in, ports.audio_out), (2, 2));
    let ([l, r], _) = run(plugin.as_mut(), 100, 0.5, &[]);
    assert!(l.iter().chain(&r).all(|&s| s == 0.5));
}

fn latency_state(magic: &[u8; 4], latency: u32) -> Vec<u8> {
    let mut state = magic.to_vec();
    state.extend_from_slice(&latency.to_le_bytes());
    state
}

/// New latency from loading state reaches the engine through `take_layout_changed` and the
/// next prepare, not behind its back.
#[test]
fn latency_changes_wait_for_the_next_prepare() {
    let mut plugin = instance();
    assert!(!plugin.take_layout_changed());
    plugin.load_state(&latency_state(b"DAWL", 64)).unwrap();
    assert_eq!(
        plugin.latency(),
        32,
        "changed before the engine prepared again"
    );
    assert!(plugin.take_layout_changed());
    assert!(!plugin.take_layout_changed(), "reported twice");
    plugin.prepare(SR, MAX);
    assert_eq!(plugin.latency(), 64);
    plugin.on_main_thread();
    assert!(!plugin.take_layout_changed());
}

/// A restart request is passed to the host instead of re-activating the plugin under the
/// engine, and the prepare that follows does not ask for another one.
#[test]
fn restart_requests_wait_for_the_next_prepare() {
    let mut plugin = instance();
    // Same latency: only the restart request is left to report.
    plugin.load_state(&latency_state(b"DAWR", 32)).unwrap();
    assert!(!plugin.take_layout_changed());
    plugin.on_main_thread();
    assert!(plugin.take_layout_changed());
    // Still processing with the old activation meanwhile.
    let (out, _) = run(plugin.as_mut(), 64, 0.5, &[]);
    assert!(out[0].iter().any(|&x| x != 0.0));

    plugin.prepare(SR, MAX);
    plugin.on_main_thread();
    assert!(!plugin.take_layout_changed());
}

/// State that changes the latency and also asks for a restart is one report, not two.
#[test]
fn a_latency_change_and_restart_from_one_load_are_reported_once() {
    let mut plugin = instance();
    plugin.load_state(&latency_state(b"DAWR", 48)).unwrap();
    assert!(plugin.take_layout_changed());
    assert_eq!(plugin.latency(), 32);
    plugin.on_main_thread();
    assert!(!plugin.take_layout_changed(), "reported twice");
    plugin.prepare(SR, MAX);
    assert_eq!(plugin.latency(), 48);
}

/// A restart request that a prepare got to first is answered by that prepare.
#[test]
fn a_prepare_answers_a_pending_restart_request() {
    let mut plugin = instance();
    plugin.load_state(&latency_state(b"DAWR", 48)).unwrap();
    plugin.prepare(SR, MAX);
    assert_eq!(plugin.latency(), 48);
    plugin.on_main_thread();
    assert!(!plugin.take_layout_changed());
}

/// The audio half plays on another thread while the main thread saves state and runs
/// callbacks, as CLAP allows; parameter changes it receives reach the main side's state.
#[test]
fn a_split_instance_processes_while_the_main_thread_works() {
    let mut plugin = instance();
    let mut half = plugin.split().expect("an active CLAP instance splits");
    let ([l, _], _) = run(plugin.as_mut(), 8, 1.0, &[]);
    assert!(
        l.iter().all(|&s| s == 0.0),
        "the instance is silent while split"
    );
    assert!(plugin.split().is_none(), "only one half at a time");

    let audio = std::thread::spawn(move || {
        let ([l, _], _) = run_with(
            &mut |ctx| half.process(ctx),
            64,
            1.0,
            &[param(GAIN, 0.5, 0)],
        );
        assert!(l.iter().all(|&s| s == 0.5));
        for _ in 0..200 {
            let ([l, _], _) = run_with(&mut |ctx| half.process(ctx), 64, 1.0, &[]);
            assert!(l.iter().all(|&s| s == 0.5));
        }
        half
    });
    for _ in 0..50 {
        plugin.on_main_thread();
        plugin.save_state().unwrap();
        let _ = plugin.param_value(GAIN);
    }
    let half = audio.join().expect("the audio thread");
    plugin.join(half);

    let state = plugin.save_state().unwrap();
    let mut other = instance();
    other.load_state(&state).unwrap();
    assert_eq!(other.param_value(GAIN), 0.5);
    // Joined, the instance plays again and can be prepared again.
    let ([l, _], _) = run(plugin.as_mut(), 8, 1.0, &[]);
    assert!(l.iter().all(|&s| s == 0.5));
    plugin.prepare(SR, MAX);
    assert!(plugin.split().is_some_and(|h| {
        plugin.join(h);
        true
    }));
}

#[test]
fn preparing_with_the_half_out_is_refused_and_changes_nothing() {
    let mut plugin = instance();
    let half = plugin.split().expect("splits");
    plugin.prepare(SR, MAX);
    // Still active with its half out: it can be joined and keeps working.
    plugin.join(half);
    let ([l, _], _) = run(plugin.as_mut(), 8, 1.0, &[]);
    assert!(l.iter().all(|&s| s == 1.0));
    plugin.prepare(SR, MAX);
    assert!(plugin.split().is_some());
}
