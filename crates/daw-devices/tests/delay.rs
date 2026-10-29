mod common;

use common::*;
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
    // Starts from the 120 BPM time and glides; the echo lands within a few
    // samples of 32000 because the smoother settles long before then.
    let at = r.channels[0].iter().position(|x| x.abs() > 0.1).unwrap();
    assert!(at.abs_diff(32000) <= 2, "{at}");
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
