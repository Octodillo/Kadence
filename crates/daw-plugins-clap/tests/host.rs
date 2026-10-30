//! Loads the `daw-clap-test-plugin` CLAP from disk and drives it through the contract traits.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use assert_no_alloc::{AllocDisabler, assert_no_alloc};
use daw_core::event::{Event, EventKind, EventList};
use daw_core::node::{ProcessContext, TransportState};
use daw_core::plugin::{PluginDescriptor, PluginFormat, PluginFormatKind, PluginInstance};
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
    let mut found = ClapFormat.scan(&path);
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
        plugin.process(&mut ctx);
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

#[test]
fn scanning_a_non_plugin_finds_nothing() {
    let path = std::env::temp_dir().join(format!("not-a-plugin-{}.clap", std::process::id()));
    std::fs::write(&path, b"nope").unwrap();
    assert!(ClapFormat.scan(&path).is_empty());
    std::fs::remove_file(path).unwrap();
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
