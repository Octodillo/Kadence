//! Keyed nodes survive graph swaps, meters and automation lanes.

mod support;

use std::sync::Arc;

use daw_core::event::EventList;
use daw_core::time::SampleTime;
use daw_engine::nodes::Strip;
use daw_engine::{
    AutomationLane, AutomationPoint, CompileError, Curve, EngineConfig, EngineController,
    EngineProcessor, GraphDesc, NodeKey, NodeMeter, RenderOptions, render,
};
use support::{Counter, Dc, Delay};

const SYNTH: NodeKey = NodeKey(0x01_0000);
const TRACK: NodeKey = NodeKey(42);

fn run(proc: &mut EngineProcessor, frames: usize) -> Vec<f32> {
    let mut out = vec![0.0; frames];
    proc.process(frames, &[], &mut [&mut out], &EventList::with_capacity(0));
    out
}

fn engine() -> (EngineController, EngineProcessor) {
    EngineController::new(EngineConfig {
        max_frames: 64,
        ..Default::default()
    })
}

#[test]
fn reused_node_keeps_its_state_across_swaps() {
    let (mut ctl, mut proc) = engine();
    let mut g = ctl.graph(0, 1);
    let synth = g.add_device("counter", Box::new(Counter(0.0)));
    g.set_key(synth, SYNTH);
    let track = g.add_track("track", 1);
    g.set_key(track, TRACK);
    g.connect(synth, track);
    ctl.set_graph(g).unwrap();
    assert_eq!(run(&mut proc, 4), vec![1.0, 2.0, 3.0, 4.0]);
    // Queued right before the swaps, so they must move with the nodes.
    ctl.set_param_by_key(TRACK, Strip::GAIN, 2.0).unwrap();
    ctl.set_param_by_key(NodeKey::MASTER, Strip::GAIN, 0.75)
        .unwrap();

    // A structural edit: a new graph with an extra track, reusing the rest.
    for round in 0..3 {
        assert!(ctl.has_node(SYNTH));
        let mut g = ctl.graph(0, 1);
        let synth = g.reuse(SYNTH).unwrap();
        let track = g.reuse(TRACK).unwrap();
        assert_eq!(g.name(track), Some("track"));
        assert!(g.reuse(SYNTH).is_none(), "a key can only be reused once");
        g.connect(synth, track);
        g.connect(track, g.master());
        let extra = g.add_track("extra", 1);
        g.set_key(extra, NodeKey(1000 + round));
        ctl.set_graph(g).unwrap();
    }
    // Gain 2 on the track and 0.75 on master both survived.
    assert_eq!(run(&mut proc, 3), vec![7.5, 9.0, 10.5]);
    assert!(
        !ctl.has_node(NodeKey(1000)),
        "only the latest graph is live"
    );
    assert!(ctl.has_node(NodeKey(1002)));
    ctl.collect_garbage();
}

#[test]
fn unkeyed_nodes_start_fresh() {
    let (mut ctl, mut proc) = engine();
    let mut g = ctl.graph(0, 1);
    let c = g.add_device("counter", Box::new(Counter(0.0)));
    g.connect(c, g.master());
    ctl.set_graph(g).unwrap();
    run(&mut proc, 10);
    let mut g = ctl.graph(0, 1);
    let c = g.add_device("counter", Box::new(Counter(0.0)));
    g.connect(c, g.master());
    ctl.set_graph(g).unwrap();
    assert_eq!(run(&mut proc, 2), vec![1.0, 2.0]);
}

#[test]
fn stale_and_duplicate_keys_are_rejected() {
    let (mut ctl, _proc) = engine();
    let mut g = ctl.graph(0, 1);
    let c = g.add_device("counter", Box::new(Counter(0.0)));
    g.set_key(c, SYNTH);
    ctl.set_graph(g).unwrap();

    let mut stale = ctl.graph(0, 1);
    stale.reuse(SYNTH).unwrap();
    ctl.set_graph(ctl.graph(0, 1)).unwrap();
    assert!(matches!(ctl.compile(stale), Err(CompileError::StaleGraph)));

    let mut g = ctl.graph(0, 1);
    let a = g.add_device("a", Box::new(Counter(0.0)));
    let b = g.add_device("b", Box::new(Counter(0.0)));
    g.set_key(a, SYNTH);
    g.set_key(b, SYNTH);
    assert!(matches!(
        ctl.compile(g),
        Err(CompileError::DuplicateKey(SYNTH))
    ));
    assert!(ctl.set_param_by_key(NodeKey(7), 0, 0.0).is_err());
}

#[test]
fn meters_follow_reused_nodes() {
    let (mut ctl, mut proc) = engine();
    let mut g = ctl.graph(0, 1);
    let dc = g.add_device("dc", Box::new(Dc(0.5)));
    let track = g.add_track("track", 1);
    g.set_key(track, TRACK);
    g.connect(dc, track);
    let meter = Arc::new(NodeMeter::new(1));
    g.set_meter(track, meter.clone());
    ctl.set_graph(g).unwrap();
    run(&mut proc, 32);
    assert_eq!(meter.take(), vec![0.5]);
    assert_eq!(meter.take(), vec![0.0]);

    let mut g = ctl.graph(0, 1);
    let dc = g.add_device("dc", Box::new(Dc(-0.25)));
    let track = g.reuse(TRACK).unwrap();
    g.connect(dc, track);
    g.connect(track, g.master());
    ctl.set_graph(g).unwrap();
    run(&mut proc, 32);
    let same = ctl.meter(TRACK).unwrap();
    assert!(Arc::ptr_eq(&same, &meter));
    assert_eq!(meter.take(), vec![0.25]);
}

#[test]
fn automation_does_not_crowd_out_notes() {
    use support::{EventProbe, NoteSource};
    // 32 linear lanes emit about 1024 events per 1024-frame block, twice the
    // default event capacity.
    let mut g = GraphDesc::new(0, 1);
    let src = g.add_device("notes", Box::new(NoteSource { frame: 500 }));
    let probe = g.add_device("probe", Box::new(EventProbe));
    g.connect_events(src, probe);
    g.connect(probe, g.master());
    for p in 0..32 {
        let pt = |t, value, curve| AutomationPoint {
            time: SampleTime(t),
            value,
            curve,
        };
        g.automate(
            probe,
            100 + p,
            AutomationLane::new(vec![
                pt(0, 0.0, Curve::Linear),
                pt(100_000, 1.0, Curve::Step),
            ]),
        );
    }
    let opts = RenderOptions {
        block_size: 1024,
        frames: 1024,
        ..Default::default()
    };
    let out = render(g, &opts).unwrap();
    // The note plus any automation events that land on the same frame.
    assert!(out[500] >= 1.0, "the note was dropped");
}

fn automated(block_size: u32, curve: Curve) -> Vec<f32> {
    let mut g = GraphDesc::new(0, 1);
    let dc = g.add_device("dc", Box::new(Dc(1.0)));
    let track = g.add_track("track", 1);
    g.connect(dc, track);
    let pt = |t, value, curve| AutomationPoint {
        time: SampleTime(t),
        value,
        curve,
    };
    g.automate(
        track,
        Strip::GAIN,
        AutomationLane::new(vec![
            pt(0, 1.0, Curve::Step),
            pt(1000, 0.5, curve),
            pt(1999, 0.0, Curve::Step),
        ]),
    );
    let opts = RenderOptions {
        block_size,
        frames: 2500,
        ..Default::default()
    };
    render(g, &opts).unwrap()
}

#[test]
fn automation_is_sample_accurate_and_block_size_independent() {
    let out = automated(64, Curve::Step);
    assert!(out[..1000].iter().all(|&v| v == 1.0));
    assert!(out[1000..1999].iter().all(|&v| v == 0.5));
    assert!(out[1999..].iter().all(|&v| v == 0.0));
    for bs in [1, 100, 256, 1024] {
        assert_eq!(automated(bs, Curve::Step), out, "block size {bs}");
    }

    let ramp = automated(64, Curve::Linear);
    assert_eq!(ramp[999], 1.0);
    assert_eq!(ramp[1000], 0.5);
    assert!(ramp[1500] < 0.5 && ramp[1500] > 0.0);
    assert!(ramp[1000..1999].windows(2).all(|w| w[1] <= w[0]));
    assert_eq!(ramp[1999], 0.0);
    for bs in [1, 100, 256, 1024] {
        assert_eq!(automated(bs, Curve::Linear), ramp, "block size {bs}");
    }
}

#[test]
fn automation_is_delayed_like_the_audio_behind_a_latent_node() {
    // A look-ahead plugin before the strip: the strip's input is 300 samples
    // late, so its gain change must be too, or it lands before the audio.
    let mut g = GraphDesc::new(0, 1);
    let dc = g.add_device("dc", Box::new(Dc(1.0)));
    let la = g.add_device("lookahead", Box::new(Delay::new(300)));
    let track = g.add_track("track", 1);
    g.connect(dc, la);
    g.connect(la, track);
    let pt = |t, value| AutomationPoint {
        time: SampleTime(t),
        value,
        curve: Curve::Step,
    };
    g.automate(
        track,
        Strip::GAIN,
        AutomationLane::new(vec![pt(0, 1.0), pt(1000, 0.0)]),
    );
    let opts = RenderOptions {
        block_size: 64,
        frames: 2000,
        ..Default::default()
    };
    let out = render(g, &opts).unwrap();
    assert!(out[..1000].iter().all(|&v| v == 1.0));
    assert!(out[1000..].iter().all(|&v| v == 0.0));
}

#[test]
fn automation_only_plays_while_the_transport_runs() {
    let mut g = GraphDesc::new(0, 1);
    let dc = g.add_device("dc", Box::new(Dc(1.0)));
    let track = g.add_track("track", 1);
    g.connect(dc, track);
    let lane = AutomationLane::new(vec![AutomationPoint {
        time: SampleTime(0),
        value: 0.25,
        curve: Curve::Step,
    }]);
    g.automate(track, Strip::GAIN, lane);
    let opts = RenderOptions {
        frames: 10,
        play: false,
        ..Default::default()
    };
    assert_eq!(render(g, &opts).unwrap(), vec![1.0; 10]);
}

#[test]
fn out_of_order_and_foreign_graphs_are_refused() {
    let (mut ctl, _proc) = engine();
    let mut g = ctl.graph(0, 1);
    let c = g.add_device("counter", Box::new(Counter(0.0)));
    g.set_key(c, SYNTH);
    ctl.set_graph(g).unwrap();

    // Two graphs built from the same base: only the first may be sent.
    let mut a = ctl.graph(0, 1);
    a.reuse(SYNTH).unwrap();
    let mut b = ctl.graph(0, 1);
    b.reuse(SYNTH).unwrap();
    let a = Box::new(ctl.compile(a).unwrap());
    let b = Box::new(ctl.compile(b).unwrap());
    assert!(ctl.send_graph(a).is_ok());
    assert!(matches!(
        ctl.send_graph(b),
        Err(daw_engine::SendError::Stale(_))
    ));

    // A graph built from one engine can't reuse into another.
    let (other, _p) = engine();
    let mut foreign = ctl.graph(0, 1);
    foreign.reuse(SYNTH).unwrap();
    assert!(matches!(
        other.compile(foreign),
        Err(CompileError::StaleGraph)
    ));
}

#[test]
fn input_node_can_be_metered() {
    let (mut ctl, mut proc) = engine();
    let mut g = ctl.graph(1, 1);
    let meter = Arc::new(NodeMeter::new(1));
    g.set_meter(g.input(), meter.clone());
    g.connect(g.input(), g.master());
    assert!(
        !g.can_reuse(NodeKey::MASTER),
        "master is already in the graph"
    );
    ctl.set_graph(g).unwrap();
    let input = [0.75f32; 16];
    let mut out = [0.0f32; 16];
    proc.process(16, &[&input], &mut [&mut out], &EventList::with_capacity(0));
    assert_eq!(meter.take(), vec![0.75]);
}

const LATE: NodeKey = NodeKey(7);

/// A counter straight into master beside a silent path through a node with
/// `latency`, so the counter's edge gets a compensation delay that long.
fn compensated(ctl: &EngineController, latency: usize) -> GraphDesc {
    let mut g = ctl.graph(0, 1);
    let counter = g.reuse(SYNTH).unwrap_or_else(|| {
        let id = g.add_device("counter", Box::new(Counter(0.0)));
        g.set_key(id, SYNTH);
        id
    });
    let silence = g.add_device("silence", Box::new(Dc(0.0)));
    let late = g.add_device("late", Box::new(Delay::new(latency)));
    g.set_key(late, LATE);
    g.connect(silence, late);
    g.connect(late, g.master());
    g.connect(counter, g.master());
    g
}

#[test]
fn latency_compensation_keeps_its_audio_across_swaps() {
    let (mut ctl, mut proc) = engine();
    ctl.set_graph(compensated(&ctl, 5)).unwrap();
    assert_eq!(
        run(&mut proc, 8),
        vec![0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 2.0, 3.0]
    );

    // A structural edit that leaves the latency alone: the five samples in
    // flight come out as if nothing happened.
    let mut g = compensated(&ctl, 5);
    let extra = g.add_track("extra", 1);
    g.connect(extra, g.master());
    ctl.set_graph(g).unwrap();
    assert_eq!(run(&mut proc, 4), vec![4.0, 5.0, 6.0, 7.0]);

    // More latency: the path waits out the difference, then carries on
    // with what it held.
    ctl.set_graph(compensated(&ctl, 7)).unwrap();
    assert_eq!(run(&mut proc, 5), vec![0.0, 0.0, 8.0, 9.0, 10.0]);

    // Less: the oldest audio is skipped, the newest kept. The line held
    // 11..=17, of which the last three fit.
    ctl.set_graph(compensated(&ctl, 3)).unwrap();
    assert_eq!(run(&mut proc, 4), vec![15.0, 16.0, 17.0, 18.0]);
    assert_eq!(proc.graph().unwrap().delay_lines(), 1);
    ctl.collect_garbage();
}

#[test]
fn compensation_on_the_input_survives_swaps_too() {
    let (mut ctl, mut proc) = engine();
    let graph = |ctl: &EngineController| {
        let mut g = ctl.graph(1, 1);
        let late = g.add_device("late", Box::new(Delay::new(3)));
        g.connect(g.input(), late);
        g.connect(late, g.master());
        // The dry path is delayed to match; both arrive together.
        g.connect(g.input(), g.master());
        g
    };
    let none = EventList::with_capacity(0);
    let feed = |proc: &mut EngineProcessor, input: [f32; 4]| {
        let mut out = [0.0; 4];
        proc.process(4, &[&input], &mut [&mut out], &none);
        out
    };
    ctl.set_graph(graph(&ctl)).unwrap();
    assert_eq!(feed(&mut proc, [1.0, 2.0, 3.0, 4.0]), [0.0, 0.0, 0.0, 2.0]);
    // The latent node is new (and empty); the dry path still holds 2, 3, 4.
    ctl.set_graph(graph(&ctl)).unwrap();
    assert_eq!(feed(&mut proc, [5.0, 6.0, 7.0, 8.0]), [2.0, 3.0, 4.0, 10.0]);
}

/// A probe with `lanes` automated parameters, playing from the start.
fn automated_probe(
    config: EngineConfig,
    lanes: impl IntoIterator<Item = AutomationLane>,
) -> (EngineController, EngineProcessor) {
    let (mut ctl, proc) = EngineController::new(config);
    let mut g = ctl.graph(0, 1);
    let probe = g.add_device("probe", Box::new(support::EventProbe));
    g.connect(probe, g.master());
    for (param, lane) in lanes.into_iter().enumerate() {
        g.automate(probe, param as u32, lane);
    }
    ctl.set_graph(g).unwrap();
    ctl.play().unwrap();
    (ctl, proc)
}

fn pt(t: i64, value: f64, curve: Curve) -> AutomationPoint {
    AutomationPoint {
        time: SampleTime(t),
        value,
        curve,
    }
}

#[test]
fn every_automated_parameter_of_a_node_gets_its_ramp() {
    // 64 ramps over a 1024-frame block are 64 * 32 events, four times the
    // default event capacity; each lane brings its own room.
    let ramp = || {
        AutomationLane::new(vec![
            pt(0, 0.0, Curve::Linear),
            pt(100_000, 1.0, Curve::Step),
        ])
    };
    let config = EngineConfig::default();
    let (ctl, mut proc) = automated_probe(config, (0..64).map(|_| ramp()));
    let out = run(&mut proc, 1024);
    for (frame, events) in out.iter().enumerate() {
        let expected = if frame % 32 == 0 { 64.0 } else { 0.0 };
        assert_eq!(*events, expected, "frame {frame}");
    }
    assert_eq!(ctl.dropped_params(), 0);
}

#[test]
fn a_lane_too_dense_to_fit_cannot_starve_the_others() {
    // The first lane changes on every sample; the second is one ramp.
    let dense = AutomationLane::new((0..64).map(|t| pt(t, t as f64, Curve::Step)).collect());
    let ramp = AutomationLane::new(vec![pt(0, 0.0, Curve::Linear), pt(64, 1.0, Curve::Step)]);
    let config = EngineConfig {
        max_frames: 64,
        event_capacity: 8,
        ..Default::default()
    };
    let (ctl, mut proc) = automated_probe(config, [dense, ramp]);
    let out = run(&mut proc, 64);
    // Both start values, then the dense lane's share of the room and no
    // more, which leaves the ramp its update at frame 32.
    assert_eq!(out[0], 2.0);
    assert_eq!(out[1..10], [1.0; 9]);
    assert_eq!(out[10..32], [0.0; 22]);
    assert_eq!(out[32], 1.0);
    assert_eq!(ctl.dropped_params(), 63 - 9);
    // The next block starts both parameters at their true values again.
    assert_eq!(run(&mut proc, 64)[0], 2.0);
}
