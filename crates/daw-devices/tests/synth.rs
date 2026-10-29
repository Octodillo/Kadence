mod common;

use common::*;
use daw_core::event::EventKind;
use daw_devices::devices::synth::*;
use daw_devices::{Device, Synth};

#[test]
fn golden() {
    let s = Setup::golden();
    let mut synth = Synth::new();
    synth.set_param(RESONANCE, 0.6);
    let events = [
        (0, note_on(48, 1.0)),
        (0, note_on(55, 0.7)),
        (300, note_on(64, 0.5)),
        (1200, param(CUTOFF, 600.0)),
        (1500, param(OSC2_WAVE, 1.0)),
        (2000, EventKind::Midi([0xE0, 0, 0x60])),
        (2500, note_off(48)),
        (2600, param(FILTER_TYPE, 2.0)),
        (3000, note_off(55)),
        (3000, note_off(64)),
        (3500, note_on(72, 1.0)),
        (5000, EventKind::Midi([0x80, 72, 0])),
    ];
    let r = render(&mut synth, 8000, &[], &events, s);
    check_golden("synth", &r.channels);
}

/// A plain sine through an open filter should be at the note's pitch.
#[test]
fn plays_in_tune() {
    let s = Setup::default();
    let mut synth = Synth::new();
    for (p, v) in [
        (OSC1_WAVE, 3.0),
        (OSC_MIX, 0.0),
        (CUTOFF, 20000.0),
        (FILTER_ENV_AMOUNT, 0.0),
        (KEY_TRACKING, 0.0),
        (RESONANCE, 0.0),
    ] {
        synth.set_param(p, v);
    }
    let r = render(&mut synth, 48000 * 2, &[], &[(0, note_on(69, 1.0))], s);
    let x = &r.channels[0][48000..];
    let crossings = x.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
    assert!(crossings.abs_diff(440) <= 1, "{crossings}");
}

/// Keys above 127 are clamped on note-on, so the note-off must clamp too.
#[test]
fn out_of_range_key_note_off_releases() {
    let s = Setup::default();
    let mut synth = Synth::new();
    let events = [(0, note_on(130, 1.0)), (1000, note_off(130))];
    render(&mut synth, 48000, &[], &events, s);
    assert_eq!(synth.active_voices(), 0);
}

#[test]
fn heavy_polyphony_stays_bounded_and_realtime_safe() {
    let s = Setup::default();
    let mut synth = Synth::new();
    synth.set_param(POLYPHONY, 8.0);
    let mut events: Vec<_> = (0..200)
        .map(|i| (i * 97, note_on(30 + (i * 7 % 60) as u8, 1.0)))
        .collect();
    events.extend(
        (0..200)
            .filter(|i| i % 3 == 0)
            .map(|i| (i * 97 + 40, note_off(30 + (i * 7 % 60) as u8))),
    );
    events.sort_by_key(|e| e.0);
    let r = render(&mut synth, 48000, &[], &events, s);
    assert!(synth.active_voices() <= 8);
    assert!(r.channels[0].iter().all(|x| x.is_finite() && x.abs() < 4.0));
}

#[test]
fn silence() {
    check_silence(&mut Synth::new());
}

#[test]
fn denormals() {
    let events = [
        (0, note_on(60, 1.0)),
        (0, note_on(67, 1.0)),
        (4800, note_off(60)),
        (4800, note_off(67)),
    ];
    check_denormals(&mut Synth::new(), &[], &events, 2.0);
}
