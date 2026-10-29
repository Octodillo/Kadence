mod common;

use common::*;
use daw_devices::devices::eq::*;
use daw_devices::{Device, ParametricEq};

#[test]
fn golden() {
    let s = Setup::golden();
    let n = 4096;
    let input = [noise(1, 0.5, n), noise(2, 0.5, n)];
    let mut eq = ParametricEq::new();
    eq.set_param(LOW_GAIN, 6.0);
    eq.set_param(PEAK1_GAIN, -9.0);
    eq.set_param(PEAK2_GAIN, 4.0);
    eq.set_param(PEAK2_Q, 4.0);
    eq.set_param(HIGH_GAIN, -6.0);
    let events = [
        (1500, param(HP_ON, 1.0)),
        (1500, param(HP_FREQ, 300.0)),
        (2500, param(LP_ON, 1.0)),
        (2500, param(LP_FREQ, 3000.0)),
        (3000, param(OUTPUT, -3.0)),
    ];
    let r = render(&mut eq, n, &input, &events, s);
    check_golden("eq", &r.channels);
}

#[test]
fn boosts_a_sine_at_the_peak() {
    let s = Setup::default();
    let mut eq = ParametricEq::new();
    eq.set_param(PEAK1_FREQ, 1000.0);
    eq.set_param(PEAK1_GAIN, 6.0);
    let x = sine(1000.0, 0.25, s.sample_rate, 48000);
    let r = render(&mut eq, 48000, std::slice::from_ref(&x), &[], s);
    let ratio = rms(&r.channels[0][24000..]) / rms(&x[24000..]);
    assert!((20.0 * ratio.log10() - 6.0).abs() < 0.05, "{ratio}");
}

#[test]
fn flat_eq_is_transparent() {
    let s = Setup::default();
    let x = noise(9, 0.5, 4800);
    let r = render(
        &mut ParametricEq::new(),
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
    check_silence(&mut ParametricEq::new());
}

#[test]
fn denormals() {
    let mut eq = ParametricEq::new();
    eq.set_param(HP_ON, 1.0);
    eq.set_param(PEAK1_GAIN, 12.0);
    eq.set_param(PEAK1_Q, 8.0);
    eq.set_param(PEAK1_FREQ, 40.0);
    check_denormals(&mut eq, &[impulse(4800)], &[], 8.0);
}
