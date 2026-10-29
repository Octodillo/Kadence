//! Golden renders of small graphs. Each is checked twice: against an
//! expected signal computed here with the same float operations, and
//! bit-for-bit against a file in `tests/golden/`. Set `DAW_BLESS=1` to
//! rewrite the files after an intended change.

mod support;

use std::path::PathBuf;

use daw_engine::nodes::Strip;
use daw_engine::{CompiledGraph, GraphDesc, NodeKind, RenderOptions, render};
use support::{Delay, Sine};

const FRAMES: usize = 1000;

fn opts(block_size: u32) -> RenderOptions {
    RenderOptions {
        sample_rate: 48_000.0,
        block_size,
        frames: FRAMES,
        play: true,
    }
}

fn check_golden(name: &str, samples: &[f32]) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(format!("{name}.f32"));
    let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
    if std::env::var_os("DAW_BLESS").is_some() {
        std::fs::write(&path, &bytes).unwrap();
        return;
    }
    let golden = std::fs::read(&path)
        .unwrap_or_else(|e| panic!("{}: {e} (run with DAW_BLESS=1)", path.display()));
    assert!(golden == bytes, "{name} differs from {}", path.display());
}

fn assert_bits_eq(actual: &[f32], expected: &[f32]) {
    assert_eq!(actual.len(), expected.len());
    for (i, (a, e)) in actual.iter().zip(expected).enumerate() {
        assert_eq!(a.to_bits(), e.to_bits(), "sample {i}: {a} != {e}");
    }
}

/// sine → track (gain 0.5) → master
fn sine_track_graph() -> GraphDesc {
    let mut g = GraphDesc::new(0, 1);
    let sine = g.add_device("sine", Box::new(Sine::new(1)));
    let track = g.add_node(
        NodeKind::Track,
        "track",
        Box::new(Strip::track(1).with_gain(0.5)),
    );
    g.connect(sine, track);
    g.connect(track, g.master());
    g
}

/// sine → track (0.5) → master, and track → send (0.25) → bus → master
fn send_bus_graph() -> GraphDesc {
    let mut g = GraphDesc::new(0, 1);
    let sine = g.add_device("sine", Box::new(Sine::new(1)));
    let track = g.add_node(
        NodeKind::Track,
        "track",
        Box::new(Strip::track(1).with_gain(0.5)),
    );
    g.connect(sine, track);
    g.connect(track, g.master());
    let bus = g.add_bus("bus", 1);
    g.add_send(track, bus, 0.25);
    g
}

/// sine → delay (37, reports latency) → master, and sine → master directly
fn latency_graph() -> GraphDesc {
    let mut g = GraphDesc::new(0, 1);
    let sine = g.add_device("sine", Box::new(Sine::new(1)));
    let delay = g.add_device("lookahead", Box::new(Delay::new(37)));
    g.connect(sine, delay);
    g.connect(delay, g.master());
    g.connect(sine, g.master());
    g
}

#[test]
fn sine_gain_master() {
    let out = render(sine_track_graph(), &opts(64)).unwrap();
    let expected: Vec<f32> = Sine::reference(FRAMES)
        .iter()
        .map(|s| (0.0 + s * 0.5) * 1.0)
        .collect();
    assert_bits_eq(&out, &expected);
    check_golden("sine_gain_master", &out);
}

#[test]
fn send_and_bus() {
    let out = render(send_bus_graph(), &opts(64)).unwrap();
    let expected: Vec<f32> = Sine::reference(FRAMES)
        .iter()
        .map(|s| {
            let track = (0.0 + s) * 0.5;
            let bus = (0.0 + track * 0.25) * 1.0;
            (0.0 + track + bus) * 1.0
        })
        .collect();
    assert_bits_eq(&out, &expected);
    check_golden("send_and_bus", &out);
}

#[test]
fn latency_is_compensated() {
    let compiled = CompiledGraph::compile(latency_graph(), 48_000.0, 64, 16).unwrap();
    assert_eq!(compiled.latency(), 37);
    assert_eq!(compiled.delay_lines(), 1);

    let out = render(latency_graph(), &opts(64)).unwrap();
    let sine = Sine::reference(FRAMES);
    let expected: Vec<f32> = (0..FRAMES)
        .map(|i| {
            if i < 37 {
                0.0
            } else {
                (0.0 + sine[i - 37] + sine[i - 37]) * 1.0
            }
        })
        .collect();
    assert_bits_eq(&out, &expected);
    check_golden("latency_compensated", &out);
}

#[test]
fn block_size_does_not_change_the_render() {
    for graph in [sine_track_graph, send_bus_graph, latency_graph] {
        let reference = render(graph(), &opts(64)).unwrap();
        for block in [1, 7, 256, 1024] {
            assert_bits_eq(&render(graph(), &opts(block)).unwrap(), &reference);
        }
    }
}

#[test]
fn stereo_pan_balance() {
    let mut g = GraphDesc::new(0, 2);
    let sine = g.add_device("sine", Box::new(Sine::new(2)));
    let track = g.add_node(
        NodeKind::Track,
        "track",
        Box::new(Strip::track(2).with_pan(0.5)),
    );
    g.connect(sine, track);
    g.connect(track, g.master());
    let out = render(g, &opts(64)).unwrap();
    let sine = Sine::reference(FRAMES);
    for i in 0..FRAMES {
        assert_eq!(out[2 * i].to_bits(), (sine[i] * 0.5).to_bits());
        assert_eq!(out[2 * i + 1].to_bits(), sine[i].to_bits());
    }
}

#[test]
fn wav_round_trip() {
    let dir = std::env::temp_dir().join(format!("daw-engine-wav-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("out.wav");
    daw_engine::render_to_wav(send_bus_graph(), &opts(64), &path).unwrap();
    let mut reader = hound::WavReader::open(&path).unwrap();
    assert_eq!(reader.spec().channels, 1);
    assert_eq!(reader.spec().sample_rate, 48_000);
    let read: Vec<f32> = reader.samples::<f32>().map(Result::unwrap).collect();
    assert_bits_eq(&read, &render(send_bus_graph(), &opts(64)).unwrap());
    std::fs::remove_dir_all(dir).unwrap();
}
