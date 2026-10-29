//! Headless hosting tests: the `testgain` example plugin always, and the VST 3 SDK's `again`
//! sample when `DAW_VST3_AGAIN` points at a built `again.vst3` bundle (CI builds it).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use assert_no_alloc::{AllocDisabler, assert_no_alloc};
use daw_core::{
    Event, EventKind, EventList, PluginDescriptor, PluginFormat, PluginInstance, ProcessContext,
    TransportState,
};
use daw_plugins_vst3::Vst3Format;

#[cfg(debug_assertions)]
#[global_allocator]
static ALLOC: AllocDisabler = AllocDisabler;

const FRAMES: u32 = 64;
const SAMPLE_RATE: f64 = 48_000.0;

/// Wraps the `testgain` example's shared object in a `.vst3` bundle.
fn testgain_bundle() -> &'static Path {
    static BUNDLE: OnceLock<PathBuf> = OnceLock::new();
    BUNDLE.get_or_init(|| {
        // target/<profile>/deps/<test binary>
        let profile_dir = std::env::current_exe()
            .unwrap()
            .parent()
            .and_then(Path::parent)
            .unwrap()
            .to_path_buf();
        let so = profile_dir.join("examples/libtestgain.so");
        if !so.exists() {
            let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
            let status = Command::new(cargo)
                .args(["build", "-p", "daw-plugins-vst3", "--example", "testgain"])
                .status()
                .expect("run cargo to build the test plugin");
            assert!(status.success(), "building the testgain example failed");
        }
        let root = std::env::temp_dir().join(format!("daw-vst3-tests-{}", std::process::id()));
        let bin = root
            .join("TestGain.vst3/Contents")
            .join(format!("{}-linux", std::env::consts::ARCH));
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::copy(&so, bin.join("TestGain.so")).unwrap();
        root.join("TestGain.vst3")
    })
}

fn testgain() -> PluginDescriptor {
    let found = Vst3Format.scan(testgain_bundle());
    assert_eq!(found.len(), 1, "{found:?}");
    found.into_iter().next().unwrap()
}

/// Runs one block of stereo audio through the plugin and returns the outputs and out events.
fn run(
    plugin: &mut dyn PluginInstance,
    input: &[f32],
    events: &[Event],
) -> ([Vec<f32>; 2], Vec<Event>) {
    let ports = plugin.ports();
    let ins: Vec<Vec<f32>> = (0..ports.audio_in).map(|_| input.to_vec()).collect();
    let mut outs: Vec<Vec<f32>> = (0..ports.audio_out)
        .map(|_| vec![9.0; input.len()])
        .collect();
    let mut events_in = EventList::with_capacity(64);
    for e in events {
        events_in.push(*e).unwrap();
    }
    let mut events_out = EventList::with_capacity(64);
    let transport = TransportState {
        playing: true,
        bpm: 120.0,
        sample_rate: SAMPLE_RATE,
        ..Default::default()
    };
    {
        let audio_in: Vec<&[f32]> = ins.iter().map(|v| v.as_slice()).collect();
        let mut audio_out: Vec<&mut [f32]> = outs.iter_mut().map(|v| v.as_mut_slice()).collect();
        let mut ctx = ProcessContext {
            frames: input.len() as u32,
            transport: &transport,
            audio_in: &audio_in,
            audio_out: &mut audio_out,
            events_in: &events_in,
            events_out: &mut events_out,
        };
        // The workspace enables `warn_debug`, so allocations are counted rather than aborting.
        let before = assert_no_alloc::violation_count();
        assert_no_alloc(|| plugin.process(&mut ctx));
        assert_eq!(
            assert_no_alloc::violation_count(),
            before,
            "process allocated"
        );
    }
    let out_events = events_out.iter().copied().collect();
    let mut outs = outs.into_iter();
    ([outs.next().unwrap(), outs.next().unwrap()], out_events)
}

fn ramp() -> Vec<f32> {
    (0..FRAMES).map(|i| i as f32 / FRAMES as f32).collect()
}

fn param(param: u32, value: f64) -> Event {
    Event {
        frame: 0,
        kind: EventKind::ParamValue { param, value },
    }
}

fn assert_scaled(out: &[f32], input: &[f32], gain: f32) {
    for (o, i) in out.iter().zip(input) {
        assert!((o - i * gain).abs() < 1e-5, "{o} != {i} * {gain}");
    }
}

#[test]
fn scans_the_test_plugin() {
    let d = testgain();
    assert_eq!(d.name, "Test Gain");
    assert_eq!(d.vendor, "Kadence Tests");
    assert_eq!(d.ports, (2, 2));
    assert!(!d.is_instrument);
    assert_eq!(d.id.len(), 32);
    // A directory scan finds the same bundle.
    let dir = testgain_bundle().parent().unwrap();
    assert_eq!(Vst3Format.scan(dir).len(), 1);
}

#[test]
fn processes_audio_params_and_events() {
    let d = testgain();
    let mut p = Vst3Format.instantiate(&d, SAMPLE_RATE, FRAMES).unwrap();
    let ports = p.ports();
    assert_eq!((ports.audio_in, ports.audio_out), (2, 2));
    assert!(ports.event_in && ports.event_out);
    assert_eq!(p.latency(), 32);

    let params = p.params();
    assert_eq!(params.len(), 2);
    assert_eq!(params[0].name, "Gain");
    assert!(params[0].automatable);
    assert!(
        !params[1].automatable,
        "read-only params are not automatable"
    );
    assert_eq!((params[0].min, params[0].max), (0.0, 1.0));

    let input = ramp();
    let (out, _) = run(p.as_mut(), &input, &[]);
    assert_scaled(&out[0], &input, 1.0);

    let note = Event {
        frame: 3,
        kind: EventKind::NoteOn {
            channel: 1,
            key: 60,
            velocity: 0.5,
            note_id: 7,
        },
    };
    let (out, events) = run(p.as_mut(), &input, &[param(0, 0.5), note]);
    assert_scaled(&out[0], &input, 0.5);
    assert_scaled(&out[1], &input, 0.5);
    assert!(events.contains(&note), "note echoed: {events:?}");
    let peak = events.iter().find_map(|e| match e.kind {
        EventKind::ParamValue { param: 1, value } => Some(value),
        _ => None,
    });
    let expected = input.last().unwrap() * 0.5;
    assert!((peak.unwrap() - expected as f64).abs() < 1e-6);

    p.on_main_thread();
    assert_eq!(p.param_value(0), 0.5);
}

#[test]
fn midi_cc_goes_through_the_plugins_mapping() {
    let mut p = Vst3Format
        .instantiate(&testgain(), SAMPLE_RATE, FRAMES)
        .unwrap();
    let cc = Event {
        frame: 0,
        kind: EventKind::Midi([0xb0, 7, 127]),
    };
    let input = ramp();
    run(p.as_mut(), &input, &[param(0, 0.0)]);
    let (out, _) = run(p.as_mut(), &input, &[cc]);
    assert_scaled(&out[0], &input, 1.0);
}

#[test]
fn state_round_trips() {
    let d = testgain();
    let mut a = Vst3Format.instantiate(&d, SAMPLE_RATE, FRAMES).unwrap();
    run(a.as_mut(), &ramp(), &[param(0, 0.25)]);
    let state = a.save_state().unwrap();

    let mut b = Vst3Format.instantiate(&d, SAMPLE_RATE, FRAMES).unwrap();
    b.load_state(&state).unwrap();
    assert_eq!(
        b.param_value(0),
        0.25,
        "controller follows the component state"
    );
    let input = ramp();
    let (out, _) = run(b.as_mut(), &input, &[]);
    assert_scaled(&out[0], &input, 0.25);
    assert_eq!(b.save_state().unwrap(), state);

    assert!(b.load_state(b"garbage").is_err());
}

#[test]
fn no_editor_is_reported_cleanly() {
    let mut p = Vst3Format
        .instantiate(&testgain(), SAMPLE_RATE, FRAMES)
        .unwrap();
    assert!(!p.has_editor());
    assert!(p.open_editor().is_err());
    p.close_editor();
}

#[test]
fn many_instances_share_one_module() {
    let d = testgain();
    let plugins: Vec<_> = (0..8)
        .map(|_| Vst3Format.instantiate(&d, SAMPLE_RATE, FRAMES).unwrap())
        .collect();
    drop(plugins);
    // And the module loads again after the last instance is gone.
    Vst3Format.instantiate(&d, SAMPLE_RATE, FRAMES).unwrap();
}

/// The SDK's `again` sample: split component/controller built from the real SDK classes.
fn again() -> Option<Vec<PluginDescriptor>> {
    match std::env::var_os("DAW_VST3_AGAIN") {
        Some(path) => {
            let found = Vst3Format.scan(Path::new(&path));
            assert!(!found.is_empty(), "no classes in {path:?}");
            Some(found)
        }
        None => {
            assert!(
                std::env::var_os("DAW_VST3_REQUIRE_SDK_PLUGINS").is_none(),
                "DAW_VST3_AGAIN is not set but SDK plugins are required"
            );
            eprintln!("DAW_VST3_AGAIN not set; the SDK again test has nothing to load");
            None
        }
    }
}

#[test]
fn sdk_again_loads_processes_automates_and_restores() {
    let Some(found) = again() else { return };
    let d = found
        .iter()
        .find(|d| !d.name.contains("SideChain"))
        .expect("plain AGain class");
    assert_eq!(d.ports, (2, 2));
    assert_eq!(d.vendor, "Steinberg Media Technologies");

    let mut p = Vst3Format.instantiate(d, SAMPLE_RATE, FRAMES).unwrap();
    let gain = p
        .params()
        .into_iter()
        .find(|i| i.name == "Gain")
        .expect("Gain parameter");
    assert!(gain.automatable);

    let input = ramp();
    let (out, events) = run(p.as_mut(), &input, &[param(gain.id, 0.5)]);
    assert_scaled(&out[0], &input, 0.5);
    assert_scaled(&out[1], &input, 0.5);
    assert!(
        events
            .iter()
            .any(|e| matches!(e.kind, EventKind::ParamValue { param: 1, .. })),
        "VU meter output parameter: {events:?}"
    );
    p.on_main_thread();
    assert!((p.param_value(gain.id) - 0.5).abs() < 1e-6);

    let state = p.save_state().unwrap();
    let mut q = Vst3Format.instantiate(d, SAMPLE_RATE, FRAMES).unwrap();
    q.load_state(&state).unwrap();
    assert!((q.param_value(gain.id) - 0.5).abs() < 1e-6);
    let (out, _) = run(q.as_mut(), &input, &[]);
    assert_scaled(&out[0], &input, 0.5);
    assert_eq!(q.has_editor(), p.has_editor());
}

#[test]
fn sdk_again_sidechain_flattens_aux_inputs() {
    let Some(found) = again() else { return };
    let Some(d) = found.iter().find(|d| d.name.contains("SideChain")) else {
        return;
    };
    let mut p = Vst3Format.instantiate(d, SAMPLE_RATE, FRAMES).unwrap();
    assert_eq!(p.ports().audio_in, 3, "stereo main plus mono aux");
    let input = ramp();
    let (out, _) = run(p.as_mut(), &input, &[param(0, 1.0)]);
    assert!(out[0].iter().all(|x| x.is_finite()));
}

#[test]
fn sdk_again_editor_opens_in_a_floating_window() {
    let Some(found) = again() else { return };
    if std::env::var_os("DISPLAY").is_none() {
        assert!(
            std::env::var_os("DAW_VST3_REQUIRE_SDK_PLUGINS").is_none(),
            "no X display for the editor test"
        );
        return;
    }
    let d = found
        .iter()
        .find(|d| !d.name.contains("SideChain"))
        .unwrap();
    let mut p = Vst3Format.instantiate(d, SAMPLE_RATE, FRAMES).unwrap();
    assert!(p.has_editor());
    p.open_editor().unwrap();
    for _ in 0..20 {
        p.on_main_thread();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    // Opening twice is a no-op, and closing tears the window down.
    p.open_editor().unwrap();
    p.close_editor();
    p.on_main_thread();
}
