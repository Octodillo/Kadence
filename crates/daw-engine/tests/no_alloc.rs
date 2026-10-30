//! The process path must never allocate or free. This binary installs
//! `assert_no_alloc`'s allocator. The workspace enables its `warn_debug`
//! feature, so instead of aborting it counts violations per thread, and each
//! test asserts the count stays at zero on the thread that processed.
//! `assert_no_alloc` checks nothing without debug assertions and leaves out
//! the items used here, so the binary is empty in release builds.
#![cfg(debug_assertions)]

mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use assert_no_alloc::{AllocDisabler, assert_no_alloc, reset_violation_count, violation_count};
use daw_core::event::{Event, EventKind, EventList};
use daw_core::time::SampleTime;
use daw_engine::nodes::Strip;
use daw_engine::{
    AutomationLane, AutomationPoint, Curve, EngineConfig, EngineController, GraphDesc, LoopRange,
    NodeId, NodeKey, NodeMeter, PluginHandle,
};
use support::{Delay, EventProbe, FakePlugin, NoteSource, Sine};

#[global_allocator]
static ALLOC: AllocDisabler = AllocDisabler;

const SWAPS: u64 = 1000;

/// Runs `f` under `assert_no_alloc` and returns how many allocations or
/// frees it made on this thread.
fn violations(f: impl FnOnce()) -> u32 {
    reset_violation_count();
    assert_no_alloc(f);
    violation_count()
}

#[test]
fn detector_catches_allocations() {
    assert!(violations(|| drop(std::hint::black_box(vec![1u8; 16]))) > 0);
}

const SINE: NodeKey = NodeKey(1);
const TRACK: NodeKey = NodeKey(2);

/// A graph with every feature the process path has: tracks, a send and
/// bus, an event edge, external events, a latency delay line, nodes reused
/// from the running graph, a meter and an automation lane.
fn graph(ctl: &EngineController, variant: u64) -> (GraphDesc, NodeId) {
    let mut g = ctl.graph(1, 1);
    let sine = g.reuse(SINE).unwrap_or_else(|| {
        let id = g.add_device("sine", Box::new(Sine::new(1)));
        g.set_key(id, SINE);
        id
    });
    let track = g.reuse(TRACK).unwrap_or_else(|| {
        let id = g.add_track("track", 1);
        g.set_key(id, TRACK);
        g.set_meter(id, Arc::new(NodeMeter::new(1)));
        id
    });
    g.connect(track, g.master());
    g.connect(sine, track);
    let ramp = |t, value| AutomationPoint {
        time: SampleTime(t),
        value,
        curve: Curve::Linear,
    };
    g.automate(
        track,
        Strip::PAN,
        AutomationLane::new(vec![ramp(0, -1.0), ramp(500, 1.0), ramp(999, 0.0)]),
    );
    g.connect(g.input(), track);
    g.route_external_events(track);
    let bus = g.add_bus("bus", 1);
    g.add_send(track, bus, 0.25);
    let delay = g.add_device("lookahead", Box::new(Delay::new(1 + variant as usize % 50)));
    g.connect(track, delay);
    g.connect(delay, g.master());
    let notes = g.add_device("notes", Box::new(NoteSource { frame: 3 }));
    let probe = g.add_device("probe", Box::new(EventProbe));
    g.connect_events(notes, probe);
    g.connect(probe, bus);
    (g, track)
}

#[test]
fn swapping_graphs_while_processing_never_allocates() {
    let (mut ctl, mut proc) = EngineController::new(EngineConfig {
        max_frames: 64,
        ..Default::default()
    });
    let first = graph(&ctl, 0).0;
    ctl.set_graph(first).unwrap();
    ctl.set_loop(Some(LoopRange {
        start: SampleTime(0),
        end: SampleTime(1000),
    }))
    .unwrap();
    ctl.play().unwrap();

    let done = Arc::new(AtomicBool::new(false));
    let rt_done = done.clone();
    let rt = std::thread::spawn(move || {
        let input = vec![0.5f32; 100];
        let mut out = vec![0.0f32; 100];
        let mut events = EventList::with_capacity(8);
        let kind = EventKind::ParamValue {
            param: Strip::GAIN,
            value: 0.8,
        };
        events.push(Event { frame: 70, kind }).unwrap();
        let mut cycles = 0u64;
        let bad = violations(|| {
            // 100 frames with max_frames 64 also exercises block splitting.
            while !rt_done.load(Ordering::Acquire) {
                proc.process(100, &[&input], &mut [&mut out], &events);
                cycles += 1;
            }
        });
        (proc, out, cycles, bad)
    });

    for i in 1..=SWAPS {
        let (g, track) = graph(&ctl, i);
        let mut compiled = Box::new(ctl.compile(g).unwrap());
        while let Err(daw_engine::SendError::Full(back)) = ctl.send_graph(compiled) {
            compiled = back;
            std::thread::yield_now();
        }
        while ctl.set_param(track, Strip::GAIN, 0.5).is_err() {
            std::thread::yield_now();
        }
    }
    while ctl.graphs_swapped() < SWAPS + 1 {
        ctl.collect_garbage();
        std::thread::yield_now();
    }
    done.store(true, Ordering::Release);
    let (proc, out, cycles, bad) = rt.join().unwrap();
    assert_eq!(bad, 0, "the process path allocated or freed");
    drop(proc);
    ctl.collect_garbage();

    assert_eq!(ctl.graphs_swapped(), SWAPS + 1);
    assert!(cycles > 0);
    assert!(out.iter().any(|&v| v != 0.0));
    let meter = ctl.meter(TRACK).expect("the track kept its meter");
    assert!(meter.take()[0] > 0.0);
}

#[test]
fn steady_state_processing_never_allocates() {
    let (mut ctl, mut proc) = EngineController::new(EngineConfig {
        max_frames: 128,
        ..Default::default()
    });
    let (g, track) = graph(&ctl, 7);
    ctl.set_graph(g).unwrap();
    ctl.play().unwrap();
    let input = vec![0.25f32; 512];
    let mut out = vec![0.0f32; 512];
    let events = EventList::with_capacity(4);
    for _ in 0..4 {
        ctl.set_param(track, Strip::PAN, 0.0).unwrap();
        ctl.locate(SampleTime(0)).unwrap();
    }
    let bad = violations(|| {
        for _ in 0..100 {
            proc.process(512, &[&input], &mut [&mut out], &events);
        }
    });
    assert_eq!(bad, 0, "the process path allocated or freed");
    assert!(out.iter().any(|&v| v != 0.0));
}

#[test]
fn plugins_never_allocate_while_the_main_thread_uses_and_restarts_them() {
    stress_plugin(FakePlugin::new(1));
}

#[test]
fn split_plugins_never_allocate_while_the_main_thread_uses_and_restarts_them() {
    stress_plugin(FakePlugin::splitting(1));
}

fn stress_plugin(fake: FakePlugin) {
    let control = fake.control.clone();
    let (mut ctl, mut proc) = EngineController::new(EngineConfig {
        max_frames: 64,
        ..Default::default()
    });
    let plugin = PluginHandle::new(Box::new(fake), ctl.config().sample_rate, 64);
    let send = |ctl: &mut EngineController| {
        let c = ctl.config().clone();
        let mut g = ctl.graph(0, 2);
        let p = g.add_device("plugin", Box::new(plugin.node(c.sample_rate, c.max_frames)));
        g.connect(p, g.master());
        let mut compiled = Box::new(ctl.compile(g).unwrap());
        while let Err(daw_engine::SendError::Full(back)) = ctl.send_graph(compiled) {
            compiled = back;
            std::thread::yield_now();
        }
    };
    send(&mut ctl);

    let done = Arc::new(AtomicBool::new(false));
    let rt_done = done.clone();
    let rt = std::thread::spawn(move || {
        let (mut l, mut r) = (vec![0.0f32; 64], vec![0.0f32; 64]);
        let events = EventList::with_capacity(0);
        violations(|| {
            while !rt_done.load(Ordering::Acquire) {
                proc.process(64, &[], &mut [&mut l, &mut r], &events);
            }
        })
    });

    // Pump, touch and restart the plugin with alternating channel counts while the audio
    // thread runs; a node that called it with the wrong count would panic there.
    for i in 0..300u32 {
        plugin.with(|p| p.save_state().unwrap());
        if i % 3 == 0 {
            control.next_outputs.store(1 + i % 2, Ordering::Relaxed);
            control.request.store(true, Ordering::Relaxed);
        }
        if plugin.pump() || i % 7 == 0 {
            send(&mut ctl);
        }
        ctl.collect_garbage();
        std::thread::yield_now();
    }
    done.store(true, Ordering::Release);
    assert_eq!(rt.join().expect("the audio thread panicked"), 0);
}
