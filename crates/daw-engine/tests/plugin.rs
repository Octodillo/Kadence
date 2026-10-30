//! Plugins keep their state across graphs and follow their layout and latency changes.

mod support;

use std::sync::atomic::Ordering::Relaxed;

use daw_core::event::EventList;
use daw_engine::{EngineConfig, EngineController, EngineProcessor, PluginHandle};
use support::FakePlugin;

const RATE: f64 = 48_000.0;
const FRAMES: u32 = 64;

fn engine() -> (EngineController, EngineProcessor) {
    EngineController::new(EngineConfig {
        sample_rate: RATE,
        max_frames: FRAMES,
        ..Default::default()
    })
}

/// Both master channels for `frames` samples.
fn run(proc: &mut EngineProcessor, frames: usize) -> [Vec<f32>; 2] {
    let (mut l, mut r) = (vec![0.0; frames], vec![0.0; frames]);
    proc.process(
        frames,
        &[],
        &mut [&mut l, &mut r],
        &EventList::with_capacity(0),
    );
    [l, r]
}

/// Sends a graph with the plugin straight into a stereo master.
fn send(ctl: &mut EngineController, plugin: &PluginHandle) {
    let mut g = ctl.graph(0, 2);
    let p = g.add_device("plugin", Box::new(plugin.node(RATE, FRAMES)));
    g.connect(p, g.master());
    ctl.set_graph(g).unwrap();
    ctl.collect_garbage();
}

#[test]
fn a_layout_change_reaches_the_next_graph_and_keeps_the_plugin_state() {
    let fake = FakePlugin::new(1);
    let control = fake.control.clone();
    let plugin = PluginHandle::new(Box::new(fake));
    let (mut ctl, mut proc) = engine();
    send(&mut ctl, &plugin);
    assert_eq!(run(&mut proc, 3), [vec![1.0, 2.0, 3.0], vec![0.0; 3]]);
    assert_eq!(control.prepares.load(Relaxed), 1);

    // A rebuild with nothing to change carries on without preparing the plugin again.
    assert!(!plugin.pump());
    send(&mut ctl, &plugin);
    assert_eq!(run(&mut proc, 2)[0], vec![4.0, 5.0]);
    assert_eq!(control.prepares.load(Relaxed), 1);

    // The plugin asks for stereo and 32 frames of latency. The running graph plays on with
    // the old layout until the host rebuilds.
    control.next_outputs.store(2, Relaxed);
    control.next_latency.store(32, Relaxed);
    control.request.store(true, Relaxed);
    assert!(plugin.pump());
    assert!(plugin.pump(), "reported until the plugin is prepared");
    assert_eq!(run(&mut proc, 1)[0], vec![6.0]);
    assert_eq!(ctl.latency(), 0);

    send(&mut ctl, &plugin);
    assert_eq!(control.prepares.load(Relaxed), 2);
    assert!(!plugin.pump());
    let [l, r] = run(&mut proc, 40);
    assert_eq!(
        ctl.latency(),
        32,
        "delay compensation follows the new latency"
    );
    // The counter carried on from 6: the same instance, prepared again, not a new one.
    assert_eq!(l, (7..47).map(|v| v as f32).collect::<Vec<_>>());
    assert_eq!(r, l, "the new output channel is routed");
}

#[test]
fn a_graph_built_before_the_change_goes_quiet_instead_of_using_the_old_layout() {
    let fake = FakePlugin::new(1);
    let control = fake.control.clone();
    let plugin = PluginHandle::new(Box::new(fake));
    let (mut ctl, mut proc) = engine();
    send(&mut ctl, &plugin);
    run(&mut proc, 2);

    control.next_outputs.store(2, Relaxed);
    control.request.store(true, Relaxed);
    assert!(plugin.pump());
    // Preparing for the next graph changes the plugin's ports, so the running graph, which
    // has one channel for it, must not call it any more (FakePlugin asserts on the count).
    let next = plugin.node(RATE, FRAMES);
    assert_eq!(run(&mut proc, 4)[0], vec![0.0; 4]);
    assert_eq!(
        plugin.busy_blocks(),
        0,
        "a retired node is not a busy plugin"
    );
    drop(next);
    send(&mut ctl, &plugin);
    assert_eq!(
        control.prepares.load(Relaxed),
        2,
        "the dropped node's prepare is kept"
    );
    assert_eq!(run(&mut proc, 2), [vec![3.0, 4.0], vec![3.0, 4.0]]);
}

#[test]
fn a_state_load_that_changes_the_layout_is_reported() {
    let fake = FakePlugin::new(2);
    let plugin = PluginHandle::new(Box::new(fake));
    let (mut ctl, mut proc) = engine();
    send(&mut ctl, &plugin);
    run(&mut proc, 4);
    let saved = plugin.with(|p| p.save_state()).unwrap();
    assert!(!plugin.layout_changed());

    plugin
        .with(|p| p.load_state(&100f32.to_le_bytes()))
        .unwrap();
    assert!(plugin.layout_changed());
    send(&mut ctl, &plugin);
    assert!(!plugin.layout_changed());
    assert_eq!(run(&mut proc, 1)[0], vec![101.0]);
    assert_eq!(saved, 4f32.to_le_bytes());
}

#[test]
fn the_audio_thread_does_not_wait_for_a_long_main_thread_call() {
    let plugin = PluginHandle::new(Box::new(FakePlugin::new(2)));
    let (mut ctl, mut proc) = engine();
    send(&mut ctl, &plugin);
    // The main thread holds the plugin for the whole cycle: the audio thread gives up on it
    // after a short wait and the block is silent.
    let held = plugin.with(|_| run(&mut proc, 8));
    assert_eq!(held, [vec![0.0; 8], vec![0.0; 8]]);
    assert_eq!(plugin.busy_blocks(), 1);
    assert_eq!(run(&mut proc, 1)[0], vec![1.0]);
}

#[test]
fn compiling_for_other_settings_prepares_the_plugin_for_them() {
    let fake = FakePlugin::new(1);
    let control = fake.control.clone();
    let plugin = PluginHandle::new(Box::new(fake));
    let (mut ctl, mut proc) = engine();
    send(&mut ctl, &plugin);
    run(&mut proc, 1);
    // A node made for 44.1 kHz, compiled into a 48 kHz engine.
    let mut g = ctl.graph(0, 2);
    let p = g.add_device("plugin", Box::new(plugin.node(44_100.0, FRAMES)));
    g.connect(p, g.master());
    assert_eq!(control.prepares.load(Relaxed), 2);
    ctl.set_graph(g).unwrap();
    assert_eq!(control.prepares.load(Relaxed), 3);
    assert_eq!(run(&mut proc, 1)[0], vec![2.0]);
}
