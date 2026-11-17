mod common;

use common::*;
use daw_core::node::Node;
use daw_core::time::Timeline;
use daw_devices::devices::delay::*;
use daw_devices::{Delay, Device};

#[test]
fn golden() {
    let s = Setup {
        bpm: 240.0,
        ..Setup::golden()
    };
    let mut d = Delay::new();
    d.set_param(DIVISION, 4.0); // 1/16 at 240 BPM: 1378 frames
    d.set_param(FEEDBACK, 0.6);
    let mut input = noise(5, 0.5, 500);
    input.resize(8000, 0.0);
    let events = [
        (3000, param(PING_PONG, 1.0)),
        (5000, param(HIGH_CUT, 2000.0)),
        (6000, param(MIX, 0.8)),
    ];
    let r = render(&mut d, 8000, &[input], &events, s);
    check_golden("delay", &r.channels);
}

#[test]
fn echo_lands_on_the_beat_division() {
    let s = Setup::default();
    let mut d = Delay::new();
    d.set_param(MIX, 1.0);
    d.set_param(FEEDBACK, 0.0);
    let r = render(&mut d, 30000, &[impulse(30000)], &[], s);
    // 1/4 at 120 BPM is half a second.
    let at = r.channels[0].iter().position(|&x| x != 0.0).unwrap();
    assert_eq!(at, 24000);
    assert!((r.channels[0][24000] - 1.0).abs() < 1e-6);
}

#[test]
fn follows_tempo() {
    let s = Setup {
        bpm: 90.0,
        ..Setup::default()
    };
    let mut d = Delay::new();
    d.set_param(MIX, 1.0);
    d.set_param(FEEDBACK, 0.0);
    let r = render(&mut d, 40000, &[impulse(40000)], &[], s);
    let at = r.channels[0].iter().position(|x| x.abs() > 0.1).unwrap();
    assert!(at.abs_diff(32000) <= 2, "{at}");
}

/// Issue #27: after a reset the delay time started at the 120 BPM value and
/// glided, so a short synced delay at another tempo echoed early.
#[test]
fn starts_at_the_transport_tempo_after_reset() {
    let s = Setup {
        bpm: 60.0,
        ..Setup::default()
    };
    let mut d = Delay::new();
    d.set_param(DIVISION, 4.0); // 1/16 at 60 BPM: 12000 frames
    d.set_param(MIX, 1.0);
    d.set_param(FEEDBACK, 0.0);
    let echo = |r: &Rendered| r.channels[0].iter().position(|x| x.abs() > 0.1).unwrap();
    let r = render(&mut d, 20000, &[impulse(20000)], &[], s);
    assert_eq!(echo(&r), 12000);
    Node::reset(&mut d);
    let r = render_prepared(
        &mut d,
        20000,
        &[impulse(20000)],
        &[],
        s,
        &Timeline::constant(60.0, 4, 4),
    );
    assert_eq!(echo(&r), 12000);
}

#[test]
fn ping_pong_alternates() {
    let s = Setup::default();
    let mut d = Delay::new();
    d.set_param(SYNC, 0.0);
    d.set_param(TIME, 100.0);
    d.set_param(MIX, 1.0);
    d.set_param(FEEDBACK, 0.5);
    d.set_param(PING_PONG, 1.0);
    d.set_param(HIGH_CUT, 20000.0);
    let r = render(&mut d, 20000, &[impulse(20000)], &[], s);
    let (l, rr) = (&r.channels[0], &r.channels[1]);
    assert!(peak(&l[4700..4900]) > 0.4 && peak(&rr[4700..4900]) == 0.0);
    assert!(peak(&rr[9500..9700]) > 0.2 && peak(&l[9500..9700]) == 0.0);
}

#[test]
fn silence() {
    check_silence(&mut Delay::new());
}

#[test]
fn denormals() {
    let mut d = Delay::new();
    d.set_param(SYNC, 0.0);
    d.set_param(TIME, 20.0);
    d.set_param(FEEDBACK, 0.9);
    check_denormals(&mut d, &[noise(4, 1.0, 4800)], &[], 10.0);
}

#[test]
fn recovers_from_non_finite_input() {
    check_recovers_from_non_finite(&mut Delay::new());
}

/// Largest change between neighbouring samples.
fn steepest(x: &[f32]) -> f32 {
    x.windows(2).fold(0.0, |m, w| m.max((w[1] - w[0]).abs()))
}

/// Issue #178: the time smoother stalled 37.5 samples short of this target
/// and then jumped the read tap there, clicking in the wet signal.
#[test]
fn time_glide_ends_without_a_click() {
    let s = Setup {
        sample_rate: 96000.0,
        ..Setup::default()
    };
    let mut d = Delay::new();
    d.set_param(SYNC, 0.0);
    d.set_param(TIME, 1000.0);
    d.set_param(MIX, 1.0);
    d.set_param(FEEDBACK, 0.0);
    let frames = 4 * 96000;
    let input = sine(20.0, 1.0, s.sample_rate, frames);
    let glide_at = 2 * 96000;
    let events = [(glide_at, param(TIME, 2000.0))];
    let r = render(&mut d, frames, std::slice::from_ref(&input), &events, s);
    // Well into the glide the tap has all but stopped, so the echo is no
    // steeper than the tone itself.
    let settling = &r.channels[0][glide_at + 9600..];
    assert!(
        steepest(settling) < 1.5 * steepest(&input),
        "{} vs {}",
        steepest(settling),
        steepest(&input)
    );
}

/// Issue #177: reset used to zero both delay buffers, megabytes of memory,
/// on the audio thread. Whatever it leaves behind must still read as silence.
#[test]
fn reset_silences_the_echoes() {
    let s = Setup::default();
    let mut d = Delay::new();
    d.set_param(MIX, 1.0);
    d.set_param(FEEDBACK, 0.9);
    render(&mut d, 30000, &[noise(3, 1.0, 30000)], &[], s);
    Node::reset(&mut d);
    let timeline = Timeline::constant(s.bpm, 4, 4);
    let r = render_prepared(&mut d, 30000, &[impulse(30000)], &[], s, &timeline);
    for ch in &r.channels {
        assert_eq!(ch.iter().position(|&x| x != 0.0), Some(24000));
        assert_eq!(ch[24000], 1.0);
    }
}
