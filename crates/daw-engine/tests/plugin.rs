//! Plugins keep their state across graphs and follow their layout and latency changes.

mod support;

use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;

use daw_core::event::EventList;
use daw_engine::{EngineConfig, EngineController, EngineProcessor, PluginHandle, PluginNode};
use support::{FakeControl, FakePlugin};

const RATE: f64 = 48_000.0;
const FRAMES: u32 = 64;

fn engine() -> (EngineController, EngineProcessor) {
    EngineController::new(EngineConfig {
        sample_rate: RATE,
        max_frames: FRAMES,
        ..Default::default()
    })
}

fn plugin(outputs: u16) -> (PluginHandle, Arc<FakeControl>) {
    let fake = FakePlugin::new(outputs);
    let control = fake.control.clone();
    (PluginHandle::new(Box::new(fake), RATE, FRAMES), control)
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

/// Sends a graph with `node` straight into a stereo master.
fn send_node(ctl: &mut EngineController, node: PluginNode) {
    let mut g = ctl.graph(0, 2);
    let p = g.add_device("plugin", Box::new(node));
    g.connect(p, g.master());
    ctl.set_graph(g).unwrap();
    ctl.collect_garbage();
}

fn send(ctl: &mut EngineController, plugin: &PluginHandle) {
    send_node(ctl, plugin.node(RATE, FRAMES));
}

fn ramp(from: u32, to: u32) -> Vec<f32> {
    (from..to).map(|v| v as f32).collect()
}

/// Asks the plugin for another output count at its next prepare.
fn ask_for(control: &FakeControl, outputs: u32) {
    control.next_outputs.store(outputs, Relaxed);
    control.request.store(true, Relaxed);
}

#[test]
fn a_layout_change_reaches_the_next_graph_and_keeps_the_plugin_state() {
    let (plugin, control) = plugin(1);
    let (mut ctl, mut proc) = engine();
    send(&mut ctl, &plugin);
    assert_eq!(run(&mut proc, 3), [ramp(1, 4), vec![0.0; 3]]);
    assert_eq!(
        control.prepares.load(Relaxed),
        0,
        "instantiate already activated it"
    );

    // A rebuild with nothing to change carries on without preparing the plugin again.
    assert!(!plugin.pump());
    send(&mut ctl, &plugin);
    assert_eq!(run(&mut proc, 2)[0], ramp(4, 6));
    assert!(!plugin.pump());
    assert_eq!(control.prepares.load(Relaxed), 0);

    // The plugin asks for stereo and 32 frames of latency. The running graph plays on with
    // the old layout until the host rebuilds.
    control.next_latency.store(32, Relaxed);
    ask_for(&control, 2);
    assert!(plugin.pump());
    assert!(plugin.pump(), "reported until the plugin is prepared");
    assert_eq!(run(&mut proc, 1)[0], vec![6.0]);
    assert_eq!(ctl.latency(), 0);

    send(&mut ctl, &plugin);
    assert_eq!(control.prepares.load(Relaxed), 1);
    let [l, r] = run(&mut proc, 40);
    assert!(!plugin.pump());
    assert_eq!(
        ctl.latency(),
        32,
        "delay compensation follows the new latency"
    );
    // The counter carried on from 6: the same instance, prepared again, not a new one.
    assert_eq!(l, ramp(7, 47));
    assert_eq!(r, l, "the new output channel is routed");
}

#[test]
fn a_graph_that_never_arrives_keeps_asking_for_a_rebuild() {
    let (plugin, control) = plugin(1);
    let (mut ctl, mut proc) = engine();
    send(&mut ctl, &plugin);
    run(&mut proc, 2);

    ask_for(&control, 2);
    assert!(plugin.pump());
    // The host prepares the plugin for its next graph, which then fails to compile or is
    // dropped. The running graph has one channel for the plugin, which now has two, so it
    // must not call it any more (FakePlugin asserts on the count)...
    drop(plugin.node(RATE, FRAMES));
    assert_eq!(run(&mut proc, 4)[0], vec![0.0; 4]);
    assert_eq!(
        plugin.busy_blocks(),
        0,
        "a retired node is not a busy plugin"
    );
    // ...and the host hears about it until it sends a graph that works.
    assert!(plugin.pump());
    send(&mut ctl, &plugin);
    assert_eq!(
        control.prepares.load(Relaxed),
        1,
        "the earlier prepare is kept"
    );
    assert_eq!(run(&mut proc, 2), [ramp(3, 5), ramp(3, 5)]);
    assert!(!plugin.pump());
}

#[test]
fn a_node_made_before_a_prepare_catches_up_when_it_is_compiled() {
    let (plugin, control) = plugin(1);
    let (mut ctl, mut proc) = engine();
    let early = plugin.node(RATE, FRAMES);
    ask_for(&control, 2);
    assert!(plugin.pump());
    drop(plugin.node(RATE, FRAMES));
    // `early` was made for one output and the graph is wired for that. Compiling it picks
    // up the plugin's new layout, so it plays rather than staying silent, and the host is
    // asked for a graph wired for the second channel.
    send_node(&mut ctl, early);
    assert_eq!(run(&mut proc, 2), [ramp(1, 3), vec![0.0; 2]]);
    assert!(plugin.pump());
    send(&mut ctl, &plugin);
    assert_eq!(run(&mut proc, 1), [vec![3.0], vec![3.0]]);
    assert!(!plugin.pump());
    assert_eq!(control.prepares.load(Relaxed), 1);
}

#[test]
fn only_the_newest_node_plays_the_plugin() {
    let (plugin, _) = plugin(1);
    let (mut ctl, mut proc) = engine();
    send(&mut ctl, &plugin);
    run(&mut proc, 2);
    // A second engine (or a second copy in one graph) takes over from its first block; the
    // first engine's node then stays quiet instead of playing the same instance.
    let (mut other, mut other_proc) = engine();
    send(&mut other, &plugin);
    assert_eq!(run(&mut other_proc, 2)[0], ramp(3, 5));
    assert_eq!(run(&mut proc, 2)[0], vec![0.0; 2]);
    // A handle serves one engine, so nothing asks the first one to rebuild; one that
    // does anyway takes the plugin back.
    send(&mut ctl, &plugin);
    assert_eq!(run(&mut proc, 2)[0], ramp(5, 7));
    assert_eq!(run(&mut other_proc, 2)[0], vec![0.0; 2]);
}

#[test]
fn a_state_load_that_changes_the_layout_is_reported() {
    let (plugin, _) = plugin(2);
    let (mut ctl, mut proc) = engine();
    send(&mut ctl, &plugin);
    run(&mut proc, 4);
    let saved = plugin.with(|p| p.save_state()).unwrap();
    assert!(!plugin.needs_rebuild());

    plugin
        .with(|p| p.load_state(&100f32.to_le_bytes()))
        .unwrap();
    assert!(plugin.needs_rebuild());
    send(&mut ctl, &plugin);
    assert!(!plugin.needs_rebuild());
    assert_eq!(run(&mut proc, 1)[0], vec![101.0]);
    assert_eq!(saved, 4f32.to_le_bytes());
}

#[test]
fn a_prepare_through_with_retires_the_running_node() {
    let (plugin, control) = plugin(1);
    let (mut ctl, mut proc) = engine();
    send(&mut ctl, &plugin);
    run(&mut proc, 1);
    control.next_outputs.store(2, Relaxed);
    plugin.with(|p| p.prepare(RATE, FRAMES));
    assert!(plugin.needs_rebuild());
    assert_eq!(run(&mut proc, 1)[0], vec![0.0]);
    send(&mut ctl, &plugin);
    assert_eq!(run(&mut proc, 1), [vec![2.0], vec![2.0]]);
}

#[test]
fn the_audio_thread_does_not_wait_for_a_long_main_thread_call() {
    let (plugin, _) = plugin(2);
    let (mut ctl, mut proc) = engine();
    send(&mut ctl, &plugin);
    // The main thread holds the plugin for the whole cycle: the audio thread gives up on it
    // after a short wait and the block is silent.
    let held = plugin.with(|_| run(&mut proc, 8));
    assert_eq!(held, [vec![0.0; 8], vec![0.0; 8]]);
    assert_eq!(plugin.busy_blocks(), 1);
    assert_eq!(run(&mut proc, 1)[0], vec![1.0]);
    assert!(!plugin.pump(), "a busy block does not need a rebuild");
}

#[test]
fn compiling_for_other_settings_prepares_the_plugin_for_them() {
    let (plugin, control) = plugin(1);
    let (mut ctl, mut proc) = engine();
    send(&mut ctl, &plugin);
    run(&mut proc, 1);
    // A node made for 44.1 kHz, compiled into a 48 kHz engine.
    let node = plugin.node(44_100.0, FRAMES);
    assert_eq!(control.prepares.load(Relaxed), 1);
    send_node(&mut ctl, node);
    assert_eq!(control.prepares.load(Relaxed), 2);
    assert_eq!(run(&mut proc, 1)[0], vec![2.0]);
}

#[test]
fn dropping_the_handle_frees_the_plugin_on_its_thread() {
    let (plugin, control) = plugin(1);
    let (mut ctl, mut proc) = engine();
    send(&mut ctl, &plugin);
    run(&mut proc, 1);
    drop(plugin);
    assert_eq!(
        *control.dropped_on.lock().unwrap(),
        Some(std::thread::current().id())
    );
    // The graph still holds its node, which now plays nothing; the processor can go to
    // another thread and drop it there.
    std::thread::spawn(move || {
        assert_eq!(run(&mut proc, 2)[0], vec![0.0; 2]);
    })
    .join()
    .unwrap();
}

#[test]
#[should_panic(expected = "from inside one of its own calls")]
fn using_the_handle_from_inside_its_own_call_panics() {
    let (plugin, _) = plugin(1);
    plugin.with(|_| plugin.pump());
}

#[test]
fn nodes_compiled_on_another_thread_are_refused() {
    let (plugin, _) = plugin(1);
    let mut node = plugin.node(RATE, FRAMES);
    let r = std::thread::spawn(move || {
        use daw_core::node::Node;
        node.prepare(RATE, FRAMES);
    })
    .join();
    assert!(r.is_err());
}

#[test]
fn a_plugin_in_no_graph_never_asks_for_a_rebuild() {
    let (plugin, control) = plugin(1);
    ask_for(&control, 2);
    assert!(!plugin.pump(), "nothing to rebuild");
    let (mut ctl, mut proc) = engine();
    send(&mut ctl, &plugin);
    run(&mut proc, 1);
    // The plugin leaves the graph while a change is pending and its node is stale.
    ask_for(&control, 1);
    assert!(plugin.pump());
    drop(plugin.node(RATE, FRAMES));
    run(&mut proc, 1);
    assert!(plugin.pump());
    ctl.set_graph(ctl.graph(0, 2)).unwrap();
    run(&mut proc, 1);
    ctl.collect_garbage();
    assert!(!plugin.pump(), "the host removed it; nothing to rebuild");
}

#[test]
fn a_second_copy_in_one_graph_stays_quiet_without_asking_for_rebuilds() {
    let (plugin, _) = plugin(1);
    let (mut ctl, mut proc) = engine();
    for _ in 0..3 {
        let mut g = ctl.graph(0, 2);
        // The newer node comes first in the graph and plays; the older one, processed
        // after it, stays quiet.
        let older = plugin.node(RATE, FRAMES);
        let newer = g.add_device("newer", Box::new(plugin.node(RATE, FRAMES)));
        let older = g.add_device("older", Box::new(older));
        g.connect_audio(newer, 0, g.master(), 0);
        g.connect_audio(older, 0, g.master(), 1);
        ctl.set_graph(g).unwrap();
        let [l, r] = run(&mut proc, 2);
        assert!(l.iter().all(|&v| v > 0.0));
        assert_eq!(r, vec![0.0; 2]);
        assert!(!plugin.pump());
        ctl.collect_garbage();
    }
}

#[test]
fn a_latency_change_from_a_state_load_keeps_the_running_graph_playing() {
    let (plugin, control) = plugin(1);
    let (mut ctl, mut proc) = engine();
    send(&mut ctl, &plugin);
    run(&mut proc, 1);
    // A format that takes the new latency at once, without a prepare.
    control.next_latency.store(16, Relaxed);
    plugin.with(|p| {
        p.load_state(&1f32.to_le_bytes()).unwrap();
        p.prepare(RATE, FRAMES);
    });
    assert!(plugin.needs_rebuild());
    assert_eq!(run(&mut proc, 1)[0], vec![2.0], "still playing");
    send(&mut ctl, &plugin);
    run(&mut proc, 1);
    assert_eq!(ctl.latency(), 16);
}
