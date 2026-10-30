mod common;

use common::*;
use daw_core::node::Node;
use daw_core::time::Timeline;
use daw_devices::devices::utility::*;
use daw_devices::{Device, Utility};

#[test]
fn golden() {
    let s = Setup::golden();
    let n = 4096;
    let input = [
        sine(440.0, 0.5, s.sample_rate, n),
        sine(660.0, 0.5, s.sample_rate, n),
    ];
    let events = [
        (1000, param(PAN, -0.7)),
        (2000, param(WIDTH, 0.0)),
        (3000, param(GAIN, -12.0)),
        (3500, param(INVERT_R, 1.0)),
    ];
    let r = render(&mut Utility::new(), n, &input, &events, s);
    check_golden("utility", &r.channels);
}

#[test]
fn gain_is_applied() {
    let mut u = Utility::new();
    u.set_param(GAIN, -6.0);
    let input = [vec![1.0; 4800]];
    let r = render(&mut u, 4800, &input, &[], Setup::default());
    assert!((r.channels[0][4799] - 0.501_187).abs() < 1e-5);
    assert_eq!(r.channels[0], r.channels[1]);
}

#[test]
fn silence() {
    check_silence(&mut Utility::new());
}

#[test]
fn denormals() {
    check_denormals(&mut Utility::new(), &[impulse(4800)], &[], 1.0);
}

/// Issue #27: loading state after `prepare` only set smoother targets, so a
/// Utility restored at -70 dB let a full-level transient through.
#[test]
fn load_state_after_prepare_does_not_glide() {
    let s = Setup::default();
    let mut quiet = Utility::new();
    quiet.set_param(GAIN, -70.0);
    let blob = quiet.save_state();

    let mut u = Utility::new();
    u.prepare(s.sample_rate, s.block as u32);
    u.load_state(&blob).unwrap();
    let input = [vec![1.0; 256]];
    let r = render_prepared(
        &mut u,
        256,
        &input,
        &[],
        s,
        &Timeline::constant(s.bpm, 4, 4),
    );
    assert!(peak(&r.channels[0]) < 1e-3, "{}", peak(&r.channels[0]));
}
