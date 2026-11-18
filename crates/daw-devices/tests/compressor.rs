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

    // The meter holds the highest reduction until read, then resets.
    let meter = c.gain_reduction_meter();
    assert!(Device::gain_reduction_meter(&c).is_some_and(|m| std::sync::Arc::ptr_eq(&m, &meter)));
    assert!(meter.take() >= c.gain_reduction_db() - 1e-6);
    assert_eq!(meter.take(), 0.0);
    let utility = daw_devices::create("daw.utility").expect("utility exists");
    assert!(utility.gain_reduction_meter().is_none());
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

#[test]
fn recovers_from_non_finite_input() {
    check_recovers_from_non_finite(&mut Compressor::new());
}

/// Issue #178: makeup and mix were applied unsmoothed, so a knob move or an
/// automation step changed the output level in one sample.
#[test]
fn makeup_and_mix_glide() {
    let s = Setup::default();
    // A steady level below the threshold, so only makeup moves the output.
    let mut c = Compressor::new();
    let r = render(
        &mut c,
        4800,
        &[vec![0.01; 4800]],
        &[(1000, param(MAKEUP, 24.0))],
        s,
    );
    let out = &r.channels[0];
    let target = 0.01 * 10f32.powf(24.0 / 20.0);
    assert_eq!(out[999], 0.01);
    assert!((out[4799] - target).abs() < 1e-4, "{}", out[4799]);
    assert!(steepest(out) < 0.01 * (target - 0.01), "{}", steepest(out));

    // A steady level above it: mix fades from the compressed to the dry signal.
    let mut c = Compressor::new();
    let r = render(
        &mut c,
        48000,
        &[vec![0.5; 48000]],
        &[(40000, param(MIX, 0.0))],
        s,
    );
    let out = &r.channels[0];
    let wet = out[39999];
    assert!(wet < 0.3, "{wet}");
    assert!((out[47999] - 0.5).abs() < 1e-5, "{}", out[47999]);
    assert!(steepest(&out[39000..]) < 0.01 * (0.5 - wet));
}

/// Loading state jumps to the saved makeup and mix instead of gliding there.
#[test]
fn load_state_after_prepare_does_not_glide() {
    let s = Setup::default();
    let mut saved = Compressor::new();
    saved.set_param(MAKEUP, 12.0);
    saved.set_param(MIX, 0.5);
    let mut c = Compressor::new();
    daw_core::node::Node::prepare(&mut c, s.sample_rate, s.block as u32);
    c.load_state(&saved.save_state()).unwrap();
    let timeline = daw_core::time::Timeline::constant(s.bpm, 4, 4);
    let r = render_prepared(&mut c, 64, &[vec![0.01; 64]], &[], s, &timeline);
    let expected = 0.01 * (0.5 + 0.5 * 10f32.powf(12.0 / 20.0));
    assert!(
        (r.channels[0][0] - expected).abs() < 1e-6,
        "{}",
        r.channels[0][0]
    );
}
