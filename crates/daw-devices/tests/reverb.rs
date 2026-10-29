mod common;

use common::*;
use daw_devices::devices::reverb::*;
use daw_devices::{Device, Reverb};

#[test]
fn golden() {
    let s = Setup::golden();
    let mut input = noise(11, 0.5, 300);
    input.resize(8000, 0.0);
    let events = [
        (2000, param(SIZE, 0.9)),
        (3000, param(DAMPING, 0.1)),
        (5000, param(WIDTH, 0.3)),
        (6000, param(MIX, 1.0)),
    ];
    let r = render(&mut Reverb::new(), 8000, &[input], &events, s);
    check_golden("reverb", &r.channels);
}

#[test]
fn tail_decays_and_size_lengthens_it() {
    let s = Setup::default();
    let energy_after = |size: f64| {
        let mut r = Reverb::new();
        r.set_param(SIZE, size);
        r.set_param(MIX, 1.0);
        let out = render(&mut r, 96000, &[impulse(96000)], &[], s);
        (
            rms(&out.channels[0][4800..24000]),
            rms(&out.channels[0][72000..]),
        )
    };
    let (early_small, late_small) = energy_after(0.2);
    let (_, late_big) = energy_after(0.95);
    assert!(early_small > 0.0 && late_small < early_small * 0.01);
    assert!(late_big > late_small * 10.0);
}

#[test]
fn pre_delay_holds_back_the_tail() {
    let s = Setup::default();
    let mut r = Reverb::new();
    r.set_param(MIX, 1.0);
    r.set_param(PRE_DELAY, 50.0);
    let out = render(&mut r, 10000, &[impulse(10000)], &[], s);
    let first = out.channels[0].iter().position(|&x| x != 0.0).unwrap();
    assert!(first >= 2400, "{first}");
}

#[test]
fn zero_width_is_mono() {
    let s = Setup::default();
    let mut r = Reverb::new();
    r.set_param(WIDTH, 0.0);
    let out = render(&mut r, 20000, &[impulse(20000)], &[], s);
    for (a, b) in out.channels[0].iter().zip(&out.channels[1]) {
        assert!((a - b).abs() < 1e-6);
    }
}

#[test]
fn silence() {
    check_silence(&mut Reverb::new());
}

#[test]
fn denormals() {
    check_denormals(&mut Reverb::new(), &[noise(8, 1.0, 4800)], &[], 12.0);
}

#[test]
fn recovers_from_non_finite_input() {
    check_recovers_from_non_finite(&mut Reverb::new());
}
