mod common;

use std::sync::Arc;

use common::*;
use daw_devices::devices::sampler::*;
use daw_devices::{Device, SampleBuffer, Sampler, Zone};

fn sampler_with(sample: Vec<f32>, rate: f64) -> Sampler {
    let mut s = Sampler::new();
    s.set_zones(vec![Zone::full(Arc::new(SampleBuffer::mono(sample, rate)))]);
    s
}

/// Length of the sounding part of a channel.
fn sounding(x: &[f32]) -> usize {
    x.iter().rposition(|&s| s != 0.0).map_or(0, |i| i + 1)
}

#[test]
fn golden() {
    let s = Setup::golden();
    let tone = sine(330.0, 0.8, s.sample_rate, 3000);
    let mut sampler = Sampler::new();
    let low = Arc::new(SampleBuffer::mono(tone.clone(), s.sample_rate));
    let high = Arc::new(SampleBuffer {
        channels: vec![tone.clone(), noise(7, 0.3, 3000)],
        sample_rate: 44100.0,
    });
    sampler.set_zones(vec![
        Zone::full(low).keys(0, 63, 60),
        Zone::full(high).keys(64, 127, 72),
    ]);
    sampler.set_param(RELEASE, 30.0);
    let events = [
        (0, note_on(60, 1.0)),
        (500, note_on(67, 0.5)),
        (900, note_on(48, 0.8)),
        (1500, note_off(60)),
        (2000, param(TUNE, 3.0)),
        (2200, note_on(72, 1.0)),
        (3000, param(MODE, MODE_ONE_SHOT)),
        (3100, note_on(61, 1.0)),
        (3300, note_off(61)),
    ];
    let r = render(&mut sampler, 6000, &[], &events, s);
    check_golden("sampler", &r.channels);
}

#[test]
fn key_mapping_transposes() {
    let s = Setup::default();
    let mut sampler = sampler_with(vec![0.5; 4800], s.sample_rate);
    let root = render(&mut sampler, 20000, &[], &[(0, note_on(60, 1.0))], s);
    let mut sampler = sampler_with(vec![0.5; 4800], s.sample_rate);
    let octave = render(&mut sampler, 20000, &[], &[(0, note_on(72, 1.0))], s);
    assert!(sounding(&root.channels[0]).abs_diff(4800) <= 2);
    assert!(sounding(&octave.channels[0]).abs_diff(2400) <= 2);
}

#[test]
fn sample_rate_is_converted() {
    let s = Setup::default();
    let mut sampler = sampler_with(vec![0.5; 2400], 24000.0);
    let r = render(&mut sampler, 20000, &[], &[(0, note_on(60, 1.0))], s);
    assert!(sounding(&r.channels[0]).abs_diff(4800) <= 2);
}

#[test]
fn one_shot_ignores_note_off_and_key() {
    let s = Setup::default();
    let mut sampler = sampler_with(vec![0.5; 4800], s.sample_rate);
    sampler.set_param(MODE, MODE_ONE_SHOT);
    let r = render(
        &mut sampler,
        20000,
        &[],
        &[(0, note_on(84, 1.0)), (100, note_off(84))],
        s,
    );
    assert!(sounding(&r.channels[0]).abs_diff(4800) <= 2);
}

#[test]
fn key_mapped_releases_on_note_off() {
    let s = Setup::default();
    let mut sampler = sampler_with(vec![0.5; 48000], s.sample_rate);
    sampler.set_param(RELEASE, 10.0);
    let r = render(
        &mut sampler,
        48000,
        &[],
        &[(0, note_on(60, 1.0)), (1000, note_off(60))],
        s,
    );
    let len = sounding(&r.channels[0]);
    assert!(len > 1000 && len < 3000, "{len}");
}

#[test]
fn voices_are_bounded() {
    let s = Setup::default();
    let mut sampler = sampler_with(vec![0.1; 48000], s.sample_rate);
    let events: Vec<_> = (0..100)
        .map(|i| (i * 10, note_on((i % 128) as u8, 1.0)))
        .collect();
    render(&mut sampler, 2000, &[], &events, s);
    assert_eq!(sampler.active_voices(), 32);
}

#[test]
fn silence() {
    check_silence(&mut sampler_with(vec![0.5; 100], 48000.0));
}

#[test]
fn denormals() {
    let mut sampler = sampler_with(noise(3, 0.5, 4800), 48000.0);
    let events = [(0, note_on(60, 1.0)), (2000, note_off(60))];
    check_denormals(&mut sampler, &[], &events, 1.0);
}

#[test]
fn loads_samples_from_files() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hit.wav");
    let data = daw_media::AudioData {
        sample_rate: 44_100,
        channels: vec![vec![0.5; 441], vec![-0.5; 441]],
    };
    daw_media::write_wav(&path, &data, daw_media::WavFormat::Float32).unwrap();

    let native = SampleBuffer::load(&path).unwrap();
    assert_eq!(native.sample_rate, 44_100.0);
    assert_eq!(native.channels, data.channels);

    let resampled = SampleBuffer::load_resampled(&path, 48_000).unwrap();
    assert_eq!(resampled.sample_rate, 48_000.0);
    assert!((resampled.len() as i64 - 480).abs() <= 1);

    assert!(SampleBuffer::load(dir.path().join("missing.wav")).is_err());
}
