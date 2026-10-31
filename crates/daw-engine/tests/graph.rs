mod support;

use daw_core::event::{Event, EventKind, EventList};
use daw_core::time::SampleTime;
use daw_engine::nodes::Strip;
use daw_engine::{
    CompileError, CompiledGraph, EngineConfig, EngineController, GraphDesc, LoopRange, NodeId,
    RenderOptions, render,
};
use support::{Delay, EventProbe, NoteSource, Position, Sine};

fn config(max_frames: u32) -> EngineConfig {
    EngineConfig {
        max_frames,
        ..Default::default()
    }
}

fn run(proc: &mut daw_engine::EngineProcessor, frames: usize, events: &EventList) -> Vec<f32> {
    let mut out = vec![0.0; frames];
    proc.process(frames, &[], &mut [&mut out], events);
    out
}

#[test]
fn rejects_cycles() {
    let mut g = GraphDesc::new(0, 1);
    let a = g.add_device("a", Box::new(Delay::new(1)));
    let b = g.add_device("b", Box::new(Delay::new(1)));
    g.connect(a, b);
    g.connect(b, a);
    assert!(matches!(
        CompiledGraph::validate(&g),
        Err(CompileError::Cycle(_))
    ));
}

#[test]
fn rejects_bad_ports_and_nodes() {
    let mut g = GraphDesc::new(0, 1);
    let sine = g.add_device("sine", Box::new(Sine::new(1)));
    g.connect_audio(sine, 3, g.master(), 0);
    assert_eq!(
        CompiledGraph::validate(&g),
        Err(CompileError::NoSuchOutput {
            node: sine,
            channel: 3
        })
    );

    let mut g = GraphDesc::new(0, 1);
    g.connect_audio(NodeId(99), 0, g.master(), 0);
    assert_eq!(
        CompiledGraph::validate(&g),
        Err(CompileError::UnknownNode(NodeId(99)))
    );

    let mut g = GraphDesc::new(0, 1);
    let sine = g.add_device("sine", Box::new(Sine::new(1)));
    g.connect_events(sine, g.master());
    assert_eq!(
        CompiledGraph::validate(&g),
        Err(CompileError::NoEventOutput(sine))
    );
}

#[test]
fn processes_in_dependency_order() {
    let mut g = GraphDesc::new(0, 1);
    let track = g.add_track("track", 1);
    let sine = g.add_device("sine", Box::new(Sine::new(1)));
    g.connect(sine, track);
    let compiled = CompiledGraph::compile(g, 48_000.0, 64, 16).unwrap();
    let order: Vec<_> = compiled.order().collect();
    let pos = |id| order.iter().position(|&n| n == id).unwrap();
    assert!(pos(sine) < pos(track));
    assert!(pos(track) < pos(NodeId(1)), "master runs last");
}

#[test]
fn external_input_reaches_master() {
    let mut g = GraphDesc::new(1, 1);
    g.connect(g.input(), g.master());
    let (mut ctl, mut proc) = EngineController::new(config(64));
    ctl.set_graph(g).unwrap();
    let input: Vec<f32> = (0..100).map(|i| i as f32).collect();
    let mut out = vec![0.0; 100];
    proc.process(
        100,
        &[&input],
        &mut [&mut out],
        &EventList::with_capacity(0),
    );
    assert_eq!(out, input);
}

#[test]
fn event_edges_deliver_events() {
    let mut g = GraphDesc::new(0, 1);
    let src = g.add_device("notes", Box::new(NoteSource { frame: 5 }));
    let probe = g.add_device("probe", Box::new(EventProbe));
    g.connect_events(src, probe);
    g.connect(probe, g.master());
    let out = render(
        g,
        &RenderOptions {
            block_size: 16,
            frames: 32,
            ..Default::default()
        },
    )
    .unwrap();
    let hits: Vec<usize> = out
        .iter()
        .enumerate()
        .filter(|(_, v)| **v == 1.0)
        .map(|(i, _)| i)
        .collect();
    assert_eq!(hits, [5, 21]);
}

#[test]
fn external_param_events_are_sample_accurate() {
    let mut g = GraphDesc::new(1, 1);
    let track = g.add_track("track", 1);
    g.connect(g.input(), track);
    g.route_external_events(track);
    let (mut ctl, mut proc) = EngineController::new(config(64));
    ctl.set_graph(g).unwrap();

    let mut events = EventList::with_capacity(4);
    let kind = EventKind::ParamValue {
        param: Strip::GAIN,
        value: 0.0,
    };
    events.push(Event { frame: 10, kind }).unwrap();
    let input = vec![1.0; 100];
    let mut out = vec![0.0; 100];
    // 100 frames with max_frames 64 runs as two blocks; the event at 10
    // lands in the first.
    proc.process(100, &[&input], &mut [&mut out], &events);
    assert!(out[..10].iter().all(|&v| v == 1.0));
    assert!(out[10..].iter().all(|&v| v == 0.0));
}

#[test]
fn set_param_reaches_node() {
    let mut g = GraphDesc::new(1, 1);
    let track = g.add_track("track", 1);
    g.connect(g.input(), track);
    let (mut ctl, mut proc) = EngineController::new(config(64));
    ctl.set_graph(g).unwrap();
    ctl.set_param(track, Strip::MUTE, 1.0).unwrap();
    let input = vec![1.0; 32];
    let mut out = vec![9.0; 32];
    proc.process(32, &[&input], &mut [&mut out], &EventList::with_capacity(0));
    assert!(out.iter().all(|&v| v == 0.0));

    ctl.set_param(NodeId(77), Strip::MUTE, 1.0).unwrap();
    proc.process(32, &[&input], &mut [&mut out], &EventList::with_capacity(0));
    assert_eq!(ctl.dropped_params(), 1);
}

#[test]
fn set_param_without_event_input_is_counted_as_dropped() {
    let mut g = GraphDesc::new(1, 1);
    let sine = g.add_device("sine", Box::new(Sine::new(1)));
    g.connect(sine, g.master());
    let input = g.input();
    let (mut ctl, mut proc) = EngineController::new(config(64));
    ctl.set_graph(g).unwrap();
    // Neither the Input node nor a node without an event input can take a
    // parameter change, so both must show up as dropped.
    ctl.set_param(sine, 0, 1.0).unwrap();
    ctl.set_param(input, 0, 1.0).unwrap();
    let mut out = vec![0.0; 32];
    proc.process(
        32,
        &[&[0.0; 32]],
        &mut [&mut out],
        &EventList::with_capacity(0),
    );
    assert_eq!(ctl.dropped_params(), 2);
}

#[test]
fn transport_loops_inside_a_block() {
    let mut g = GraphDesc::new(0, 1);
    let pos = g.add_device("pos", Box::new(Position));
    g.connect(pos, g.master());
    let (mut ctl, mut proc) = EngineController::new(config(64));
    ctl.set_graph(g).unwrap();
    ctl.set_loop(Some(LoopRange {
        start: SampleTime(10),
        end: SampleTime(100),
    }))
    .unwrap();
    ctl.locate(SampleTime(10)).unwrap();
    ctl.play().unwrap();

    let none = EventList::with_capacity(0);
    let out: Vec<f32> = (0..4).flat_map(|_| run(&mut proc, 64, &none)).collect();
    let expected: Vec<f32> = (0..256).map(|i| (10 + i % 90) as f32).collect();
    assert_eq!(out, expected);
    assert_eq!(ctl.position(), SampleTime(10 + 256 % 90));
    assert!(ctl.is_playing());

    ctl.stop().unwrap();
    assert!(run(&mut proc, 8, &none).iter().all(|&v| v == -1.0));
    assert!(!ctl.is_playing());
}

#[test]
fn no_graph_outputs_silence() {
    let (_ctl, mut proc) = EngineController::new(config(64));
    let mut out = vec![1.0; 64];
    proc.process(64, &[], &mut [&mut out], &EventList::with_capacity(0));
    assert!(out.iter().all(|&v| v == 0.0));
}

#[test]
fn retired_graphs_come_back_to_the_controller() {
    let (mut ctl, mut proc) = EngineController::new(config(64));
    let none = EventList::with_capacity(0);
    for _ in 0..3 {
        ctl.set_graph(GraphDesc::new(0, 1)).unwrap();
        run(&mut proc, 64, &none);
    }
    assert_eq!(ctl.graphs_swapped(), 3);
    // `set_graph` collects as it goes, so only the last retired graph is
    // still waiting.
    assert_eq!(ctl.collect_garbage(), 1);
    assert_eq!(ctl.collect_garbage(), 0);
}

#[test]
fn reports_running_latency() {
    let mut g = GraphDesc::new(0, 1);
    let d = g.add_device("d", Box::new(Delay::new(12)));
    g.connect(d, g.master());
    let (mut ctl, mut proc) = EngineController::new(config(64));
    ctl.set_graph(g).unwrap();
    run(&mut proc, 16, &EventList::with_capacity(0));
    assert_eq!(ctl.latency(), 12);
}

/// Reports `before` output channels until it is prepared, then `after`, as a
/// plugin that renegotiates its buses on activation does.
struct Shrinks {
    before: u16,
    after: u16,
    prepared: bool,
}

impl daw_core::node::Node for Shrinks {
    fn ports(&self) -> daw_core::node::PortLayout {
        daw_core::node::PortLayout {
            audio_out: if self.prepared {
                self.after
            } else {
                self.before
            },
            ..Default::default()
        }
    }

    fn prepare(&mut self, _sample_rate: f64, _max_frames: u32) {
        self.prepared = true;
    }

    fn process(&mut self, _ctx: &mut daw_core::node::ProcessContext) {}
}

#[test]
fn checks_ports_again_after_prepare() {
    let mut g = GraphDesc::new(0, 2);
    let n = g.add_device(
        "n",
        Box::new(Shrinks {
            before: 2,
            after: 1,
            prepared: false,
        }),
    );
    g.connect(n, g.master());
    assert!(CompiledGraph::validate(&g).is_ok());
    assert!(matches!(
        CompiledGraph::compile(g, 48_000.0, 64, 16),
        Err(CompileError::NoSuchOutput { channel: 1, .. })
    ));

    let mut g = GraphDesc::new(0, 1);
    g.add_device(
        "n",
        Box::new(Shrinks {
            before: 1,
            after: 65,
            prepared: false,
        }),
    );
    assert!(matches!(
        CompiledGraph::compile(g, 48_000.0, 64, 16),
        Err(CompileError::TooManyChannels { channels: 65, .. })
    ));
}

#[test]
fn send_graph_refuses_a_graph_built_for_another_config() {
    let (mut ctl, mut proc) = EngineController::new(config(1024));
    let small = CompiledGraph::compile(GraphDesc::new(0, 1), 48_000.0, 64, 16).unwrap();
    assert!(matches!(
        ctl.send_graph(Box::new(small)),
        Err(daw_engine::SendError::Config(_))
    ));
    let other_rate = CompiledGraph::compile(GraphDesc::new(0, 1), 44_100.0, 1024, 16).unwrap();
    assert!(matches!(
        ctl.send_graph(Box::new(other_rate)),
        Err(daw_engine::SendError::Config(_))
    ));
    let larger = CompiledGraph::compile(GraphDesc::new(0, 1), 48_000.0, 2048, 16).unwrap();
    assert!(ctl.send_graph(Box::new(larger)).is_ok());
    run(&mut proc, 512, &EventList::with_capacity(0));
    assert_eq!(ctl.graphs_swapped(), 1);
}

#[test]
fn zero_max_frames_does_not_hang_the_processor() {
    let (ctl, mut proc) = EngineController::new(config(0));
    assert!(matches!(
        ctl.compile(GraphDesc::new(0, 1)),
        Err(CompileError::InvalidConfig)
    ));
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let out = run(&mut proc, 64, &EventList::with_capacity(0));
        let _ = tx.send(out);
    });
    let out = rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("process returned");
    assert!(out.iter().all(|&v| v == 0.0));
}

#[test]
fn strip_ignores_nan_params() {
    let mut g = GraphDesc::new(2, 2);
    let track = g.add_track("track", 2);
    g.connect(g.input(), track);
    let (mut ctl, mut proc) = EngineController::new(config(64));
    ctl.set_graph(g).unwrap();
    ctl.set_param(track, Strip::GAIN, f64::NAN).unwrap();
    ctl.set_param(track, Strip::PAN, f64::NAN).unwrap();
    let input = vec![0.5; 32];
    let (mut l, mut r) = (vec![9.0; 32], vec![9.0; 32]);
    proc.process(
        32,
        &[&input, &input],
        &mut [&mut l, &mut r],
        &EventList::with_capacity(0),
    );
    assert_eq!(l, input);
    assert_eq!(r, input);
}

/// Passes audio through and reports whatever latency it is given, like a
/// misbehaving plugin, without allocating a buffer of that size.
struct ReportsLatency(u32);

impl daw_core::node::Node for ReportsLatency {
    fn ports(&self) -> daw_core::node::PortLayout {
        daw_core::node::PortLayout {
            audio_in: 1,
            audio_out: 1,
            ..Default::default()
        }
    }

    fn prepare(&mut self, _sample_rate: f64, _max_frames: u32) {}

    fn process(&mut self, ctx: &mut daw_core::node::ProcessContext) {
        let n = ctx.frames as usize;
        ctx.audio_out[0][..n].copy_from_slice(&ctx.audio_in[0][..n]);
    }

    fn latency(&self) -> u32 {
        self.0
    }
}

/// A source feeding the master both directly and through a node with
/// `latency`, so the direct edge needs a delay line of that length.
fn parallel_paths(latency: u32) -> GraphDesc {
    let mut g = GraphDesc::new(0, 1);
    let sine = g.add_device("sine", Box::new(Sine::new(1)));
    let late = g.add_device("late", Box::new(ReportsLatency(latency)));
    g.connect(sine, late);
    g.connect(sine, g.master());
    g.connect(late, g.master());
    g
}

#[test]
fn clamps_unbounded_plugin_latency() {
    // 0xFFFFFFFF from getLatencySamples would otherwise ask for a 16 GiB
    // delay line on the direct edge and abort the process. The graph must
    // still compile so the project keeps playing.
    let max = (48_000.0 * daw_engine::MAX_LATENCY_SECONDS) as u32;
    let g = parallel_paths(u32::MAX);
    let compiled = CompiledGraph::compile(g, 48_000.0, 64, 16).unwrap();
    assert_eq!(compiled.latency(), max);

    // Latency that only adds up past the limit along a chain is clamped too,
    // so a delay line for a parallel edge stays bounded.
    let mut g = GraphDesc::new(0, 1);
    let sine = g.add_device("sine", Box::new(Sine::new(1)));
    let a = g.add_device("a", Box::new(ReportsLatency(max / 2 + 1)));
    let b = g.add_device("b", Box::new(ReportsLatency(max / 2 + 1)));
    g.connect(sine, a);
    g.connect(a, b);
    g.connect(b, g.master());
    g.connect(sine, g.master());
    let compiled = CompiledGraph::compile(g, 48_000.0, 64, 16).unwrap();
    assert_eq!(compiled.latency(), max);

    // A large but plausible latency is compensated exactly.
    let g = parallel_paths(max - 1);
    let compiled = CompiledGraph::compile(g, 48_000.0, 64, 16).unwrap();
    assert_eq!(compiled.latency(), max - 1);
}

/// One block through a track routed by `route`, with a gain-to-zero event
/// in source `source`; returns the last output sample.
fn gain_event_from(route: impl FnOnce(&mut GraphDesc, NodeId), source: usize) -> f32 {
    let mut g = GraphDesc::new(1, 1);
    let track = g.add_track("track", 1);
    g.connect(g.input(), track);
    route(&mut g, track);
    let (mut ctl, mut proc) = EngineController::new(EngineConfig {
        event_sources: 2,
        ..config(64)
    });
    ctl.set_graph(g).unwrap();
    let mut gain = EventList::with_capacity(1);
    let kind = EventKind::ParamValue {
        param: Strip::GAIN,
        value: 0.0,
    };
    gain.push(Event { frame: 0, kind }).unwrap();
    let none = EventList::with_capacity(0);
    let mut sources = [&none; 6];
    sources[source] = &gain;
    let input = vec![1.0; 32];
    let mut out = vec![0.0; 32];
    proc.process_sources(32, &[&input], &mut [&mut out], &sources);
    out[31]
}

#[test]
fn external_events_can_come_from_one_source() {
    let from = |s| move |g: &mut GraphDesc, t| g.route_external_events_from(t, s);
    assert_eq!(gain_event_from(from(1), 1), 0.0, "its own source");
    assert_eq!(gain_event_from(from(1), 0), 1.0, "another source");
    let all = |g: &mut GraphDesc, t| g.route_external_events(t);
    assert_eq!(gain_event_from(all, 0), 0.0);
    assert_eq!(gain_event_from(all, 1), 0.0);
    // Past `event_sources`, a source reaches only nodes taking all of them.
    assert_eq!(gain_event_from(all, 5), 0.0);
    assert_eq!(gain_event_from(from(5), 5), 1.0);
    assert_eq!(gain_event_from(from(1), 5), 1.0);
}
