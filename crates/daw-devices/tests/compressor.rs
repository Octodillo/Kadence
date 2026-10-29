mod common;

use common::*;
use daw_devices::devices::compressor::*;
use daw_devices::{Compressor, Device};

#[test]
fn golden() {
    let s = Setup::golden();
    let n = 6000;
    let mut loud = sine(200.0, 0.9, s.sample_rate, n);
    for x in &mut loud[3000..] {
        *x *= 0.1;
    }
    let events = [
        (1000, param(RATIO, 10.0)),
        (2000, param(MAKEUP, 6.0)),
        (4500, param(MIX, 0.5)),
    ];
    let r = render(&mut Compressor::new(), n, &[loud], &events, s);
    check_golden("compressor", &r.channels);
}

#[test]
fn steady_state_follows_the_static_curve() {
    let s = Setup::default();
    let mut c = Compressor::new();
    c.set_param(THRESHOLD, -18.0);
    c.set_param(RATIO, 4.0);
    c.set_param(KNEE, 0.0);
    // A square wave has a constant peak level, so the detector settles.
    let x: Vec<f32> = (0..48000)
        .map(|i| if (i / 50) % 2 == 0 { 0.5 } else { -0.5 })
        .collect();
    let r = render(&mut c, 48000, &[x], &[], s);
    let out_db = 20.0 * peak(&r.channels[0][40000..]).log10();
    let in_db = 20.0 * 0.5f32.log10();
    let expected = -18.0 + (in_db + 18.0) / 4.0;
    assert!((out_db - expected).abs() < 0.1, "{out_db} vs {expected}");
    assert!((c.gain_reduction_db() - (in_db - expected)).abs() < 0.1);
}

#[test]
fn quiet_signals_pass_unchanged() {
    let s = Setup::default();
    let x = sine(500.0, 0.01, s.sample_rate, 4800);
    let r = render(
        &mut Compressor::new(),
        4800,
        std::slice::from_ref(&x),
        &[],
        s,
    );
    for (a, b) in r.channels[0].iter().zip(&x) {
        assert!((a - b).abs() < 1e-6);
    }
}

#[test]
fn silence() {
    check_silence(&mut Compressor::new());
}

#[test]
fn denormals() {
    check_denormals(
        &mut Compressor::new(),
        &[sine(100.0, 1.0, 48000.0, 4800)],
        &[],
        2.0,
    );
}
