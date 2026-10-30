mod common;

use common::*;
use daw_core::node::Node;
use daw_core::time::{Meter, MeterPoint, PPQ, SegmentTempoMap, TickTime, Timeline};
use daw_devices::Metronome;
use daw_devices::devices::metronome::*;

fn onsets(x: &[f32]) -> Vec<usize> {
    let mut v = Vec::new();
    let mut quiet = usize::MAX / 2;
    for (i, s) in x.iter().enumerate() {
        if *s != 0.0 && quiet > 100 {
            v.push(i);
        }
        quiet = if *s == 0.0 { quiet + 1 } else { 0 };
    }
    v
}

#[test]
fn golden() {
    let s = Setup {
        bpm: 480.0,
        ..Setup::golden()
    };
    let events = [
        (6000, param(BEATS_PER_BAR, 3.0)),
        (7000, param(PITCH, 1500.0)),
    ];
    let r = render(&mut Metronome::new(), 11025, &[], &events, s);
    check_golden("metronome", &r.channels);
}

#[test]
fn clicks_on_beats_with_accent() {
    let s = Setup::default();
    let r = render(&mut Metronome::new(), 100_000, &[], &[], s);
    // One click per half second at 120 BPM, with the bar's first beat
    // (every fourth) louder.
    // Each click starts at phase 0, so the first nonzero sample is one later.
    let found: Vec<usize> = onsets(&r.channels[0]).iter().map(|i| i - 1).collect();
    assert_eq!(found, vec![0, 24000, 48000, 72000, 96000]);
    let p = |at: usize| peak(&r.channels[0][at..at + 2000]);
    assert!(p(0) > p(24000) * 1.3);
    assert!((p(0) - p(96000)).abs() < 1e-6);
}

#[test]
fn silent_when_stopped() {
    let s = Setup {
        playing: false,
        ..Setup::default()
    };
    let r = render(&mut Metronome::new(), 48000, &[], &[], s);
    assert!(r.channels.iter().all(|c| c.iter().all(|&x| x == 0.0)));
}

#[test]
fn denormals() {
    // Plays one click (beat 0 at 6 BPM is the only one in 3 s), then decays.
    let mut m = Metronome::new();
    let s = Setup {
        bpm: 6.0,
        ..Setup::default()
    };
    let r = render(&mut m, 144_000, &[], &[], s);
    assert!(r.channels[0][..1000].iter().any(|&x| x != 0.0));
    assert!(r.channels[0][20_000..].iter().all(|&x| x == 0.0));
    assert!(!r.channels[0].iter().any(|x| x.is_subnormal()));
}

fn clicks(r: &Rendered) -> Vec<usize> {
    // Each click starts at phase 0, so the first nonzero sample is one later.
    onsets(&r.channels[0]).iter().map(|i| i - 1).collect()
}

#[test]
fn tempo_changes_keep_the_beat_grid() {
    // Issue #45: 120 bpm, then 160 from beat 4. The grid continues from
    // beat 4 at the new tempo instead of jumping to where 160 bpm from the
    // start would put it.
    let mut tempo = SegmentTempoMap::new(120.0);
    tempo.set_tempo(TickTime(4 * PPQ), 160.0, false);
    let timeline = Timeline::new(tempo, Meter::default());
    for block in [64, 97] {
        let s = Setup {
            block,
            ..Setup::default()
        };
        let mut m = Metronome::new();
        m.prepare(s.sample_rate, block as u32);
        let r = render_prepared(&mut m, 140_000, &[], &[], s, &timeline);
        assert_eq!(
            clicks(&r),
            vec![0, 24000, 48000, 72000, 96000, 114000, 132000],
            "block {block}"
        );
    }
}

#[test]
fn accents_follow_the_time_signature() {
    // 6/8: a click every eighth note, the downbeat of each bar accented.
    let s = Setup::default();
    let timeline = Timeline::constant(120.0, 6, 8);
    let mut m = Metronome::new();
    m.prepare(s.sample_rate, s.block as u32);
    let r = render_prepared(&mut m, 150_000, &[], &[], s, &timeline);
    let found = clicks(&r);
    assert_eq!(found, (0..13).map(|i| i * 12_000).collect::<Vec<_>>());
    let p = |at: usize| peak(&r.channels[0][at..at + 2000]);
    for &at in &found {
        let downbeat = at % 72_000 == 0;
        assert_eq!(p(at) > p(12_000) * 1.3, downbeat, "click at {at}");
    }
}

#[test]
fn meter_changes_move_the_accent() {
    // One bar of 4/4, then 3/4.
    let meter = Meter::new([
        MeterPoint {
            tick: TickTime(0),
            numerator: 4,
            denominator: 4,
        },
        MeterPoint {
            tick: TickTime(4 * PPQ),
            numerator: 3,
            denominator: 4,
        },
    ]);
    let timeline = Timeline::new(SegmentTempoMap::new(120.0), meter);
    let s = Setup::default();
    let mut m = Metronome::new();
    m.prepare(s.sample_rate, s.block as u32);
    let r = render_prepared(&mut m, 240_000, &[], &[], s, &timeline);
    let found = clicks(&r);
    assert_eq!(found, (0..10).map(|i| i * 24_000).collect::<Vec<_>>());
    let p = |at: usize| peak(&r.channels[0][at..at + 2000]);
    let accented: Vec<usize> = found
        .into_iter()
        .filter(|&at| p(at) > p(24_000) * 1.3)
        .collect();
    assert_eq!(accented, vec![0, 96_000, 168_000]);
}

#[test]
fn a_jump_back_clicks_again() {
    let s = Setup::default();
    let timeline = Timeline::default();
    let mut m = Metronome::new();
    m.prepare(s.sample_rate, s.block as u32);
    // The second render starts over at sample 0, like a loop.
    let first = render_prepared(&mut m, 30_000, &[], &[], s, &timeline);
    let second = render_prepared(&mut m, 30_000, &[], &[], s, &timeline);
    assert_eq!(clicks(&first), vec![0, 24000]);
    assert_eq!(clicks(&second), vec![0, 24000]);
}

#[test]
fn long_blocks_across_a_meter_change_click_on_the_new_beats() {
    // One bar of 6/8, then 4/4, in blocks longer than a quarter note: the
    // block that crosses the change must not keep clicking eighths.
    let meter = Meter::new([
        MeterPoint {
            tick: TickTime(0),
            numerator: 6,
            denominator: 8,
        },
        MeterPoint {
            tick: TickTime(3 * PPQ),
            numerator: 4,
            denominator: 4,
        },
    ]);
    let timeline = Timeline::new(SegmentTempoMap::new(120.0), meter);
    let s = Setup {
        block: 30_000,
        ..Setup::default()
    };
    let mut m = Metronome::new();
    m.prepare(s.sample_rate, s.block as u32);
    let r = render_prepared(&mut m, 130_000, &[], &[], s, &timeline);
    // Eighths for the 6/8 bar, then quarters (at most a block late).
    let found = clicks(&r);
    assert_eq!(
        &found[..7],
        &[0, 12_000, 24_000, 36_000, 48_000, 60_000, 72_000]
    );
    assert!(found[7..].iter().all(|&at| at >= 96_000), "{found:?}");
    assert_eq!(found.len(), 9, "{found:?}");
}

#[test]
fn silent_before_the_project_start() {
    let s = Setup::default();
    let timeline = Timeline::default();
    let mut m = Metronome::new();
    m.prepare(s.sample_rate, s.block as u32);
    let mut out = [vec![0.0f32; 64], vec![0.0f32; 64]];
    for block in 0..10 {
        let t = daw_core::node::TransportState::at(
            &timeline,
            true,
            daw_core::time::SampleTime(-24_000 + block * 64),
            s.sample_rate,
        );
        let [l, r] = &mut out;
        let mut outs: [&mut [f32]; 2] = [l, r];
        let events_in = daw_core::event::EventList::with_capacity(0);
        let mut events_out = daw_core::event::EventList::with_capacity(0);
        m.process(&mut daw_core::node::ProcessContext {
            frames: 64,
            transport: &t,
            audio_in: &[],
            audio_out: &mut outs,
            events_in: &events_in,
            events_out: &mut events_out,
        });
        assert!(out[0].iter().all(|&x| x == 0.0), "block {block}");
    }
}
