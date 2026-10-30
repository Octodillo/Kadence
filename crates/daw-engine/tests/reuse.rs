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
use support::{Counter, Dc};

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
