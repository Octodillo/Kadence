mod common;

use common::*;
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
