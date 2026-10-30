mod common;

use std::sync::Arc;

use common::*;
use daw_core::event::EventKind;
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
        source: None,
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
fn note_off_with_id_releases_note_without_id() {
    let s = Setup::default();
    let mut sampler = sampler_with(vec![0.5; 48000], s.sample_rate);
    sampler.set_param(RELEASE, 10.0);
    let off = EventKind::NoteOff {
        channel: 0,
        key: Some(60),
        velocity: 0.0,
        note_id: 7,
    };
    let events = [(0, EventKind::Midi([0x90, 60, 100])), (1000, off)];
    render(&mut sampler, 48000, &[], &events, s);
    assert_eq!(sampler.active_voices(), 0);
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

/// Stealing a sounding voice fades it out rather than cutting it mid-cycle.
#[test]
fn stealing_does_not_click() {
    let s = Setup::default();
    let mut sampler = sampler_with(sine(100.0, 0.05, s.sample_rate, 48000), s.sample_rate);
    let events: Vec<_> = (0..40).map(|i| (i * 100, note_on(60, 1.0))).collect();
    let r = render(&mut sampler, 6000, &[], &events, s);
    let step = |x: &[f32]| {
        x.windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0, f32::max)
    };
    let x = &r.channels[0];
    // Every voice plays the same quiet sine, offset in time; 32 of them sum to a
    // smooth signal whose largest step comes just before the first steal.
    let before = step(&x[..3200]);
    let after = step(&x[3200..]);
    assert!(
        after < before * 1.5,
        "step {after} after stealing vs {before}"
    );
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

fn write_hit(dir: &std::path::Path, name: &str, level: f32) -> std::path::PathBuf {
    let path = dir.join(name);
    let data = daw_media::AudioData {
        sample_rate: 48_000,
        channels: vec![vec![level; 4800]],
    };
    daw_media::write_wav(&path, &data, daw_media::WavFormat::Float32).unwrap();
    path
}

fn kit(dir: &std::path::Path) -> Sampler {
    let kick = Arc::new(SampleBuffer::load(write_hit(dir, "kick.wav", 0.5)).unwrap());
    let snare = Arc::new(SampleBuffer::load(write_hit(dir, "snare.wav", 0.25)).unwrap());
    let mut snare_zone = Zone::full(snare).keys(38, 38, 38);
    snare_zone.gain_db = 3.0;
    let mut soft = Zone::full(kick.clone()).keys(42, 42, 42);
    soft.velocity_hi = 0.5;
    let mut hard = Zone::full(kick.clone()).keys(42, 42, 42);
    hard.velocity_lo = 0.5;
    hard.gain_db = -6.0;
    let mut s = Sampler::new();
    s.set_param(MODE, MODE_ONE_SHOT);
    s.set_zones(vec![
        Zone::full(kick).keys(36, 36, 36),
        snare_zone,
        soft,
        hard,
    ]);
    s
}

/// Source, root, key range, velocity range and gain of each zone.
type Mapping = (Option<std::path::PathBuf>, u8, u8, u8, f32, f32, f32);

fn mapping(s: &Sampler) -> Vec<Mapping> {
    s.zones()
        .iter()
        .map(|z| {
            (
                z.source.clone(),
                z.root_key,
                z.key_lo,
                z.key_hi,
                z.velocity_lo,
                z.velocity_hi,
                z.gain_db,
            )
        })
        .collect()
}

/// Issue #23: saving and reopening a sampler used to drop the zone map, leaving
/// the instrument silent.
#[test]
fn saved_state_keeps_zone_map() {
    let dir = tempfile::tempdir().unwrap();
    let a = kit(dir.path());
    let blob = a.save_state();

    let mut b = Sampler::new();
    b.load_state(&blob).unwrap();
    assert_eq!(mapping(&b), mapping(&a));
    assert_eq!(b.param_value(MODE), MODE_ONE_SHOT);
    // The same file is decoded once and shared between zones.
    assert!(Arc::ptr_eq(&b.zones()[0].sample, &b.zones()[2].sample));
    assert_eq!(b.zones()[1].sample.channels, a.zones()[1].sample.channels);

    let s = Setup::default();
    let r = render(&mut b, 4800, &[], &[(0, note_on(38, 1.0))], s);
    let expected = 0.25 * 10f32.powf(3.0 / 20.0);
    assert!(
        (r.channels[0][100] - expected).abs() < 1e-3,
        "{}",
        r.channels[0][100]
    );
}

#[test]
fn missing_sample_keeps_its_zone_and_path() {
    let dir = tempfile::tempdir().unwrap();
    let blob = kit(dir.path()).save_state();
    std::fs::remove_file(dir.path().join("snare.wav")).unwrap();

    let mut b = Sampler::new();
    b.load_state(&blob).unwrap();
    assert_eq!(b.zones().len(), 4);
    assert!(b.zones()[1].sample.is_empty());
    // Saving again keeps the reference, so restoring the file brings it back.
    write_hit(dir.path(), "snare.wav", 0.25);
    let mut c = Sampler::new();
    c.load_state(&b.save_state()).unwrap();
    assert_eq!(mapping(&c), mapping(&kit(dir.path())));
    assert!(!c.zones()[1].sample.is_empty());
}

#[test]
fn load_state_with_resolves_each_path_once() {
    let dir = tempfile::tempdir().unwrap();
    let blob = kit(dir.path()).save_state();
    let mut asked = Vec::new();
    let mut b = Sampler::new();
    b.load_state_with(&blob, |path| {
        asked.push(path.file_name().unwrap().to_owned());
        Some(Arc::new(SampleBuffer::mono(vec![0.1; 10], 48_000.0)))
    })
    .unwrap();
    assert_eq!(asked, ["kick.wav", "snare.wav"]);
    assert_eq!(b.zones().len(), 4);
}

#[test]
fn zone_without_source_is_saved_as_placeholder() {
    let mut a = sampler_with(vec![0.5; 10], 48_000.0);
    let mut b = Sampler::new();
    b.load_state(&a.save_state()).unwrap();
    assert_eq!(mapping(&b), mapping(&a));
    assert!(b.zones()[0].sample.is_empty());
    // Non-finite mapping values are sanitized, so the state stays loadable.
    let mut z = a.zones()[0].clone();
    z.gain_db = f32::NAN;
    z.velocity_hi = f32::INFINITY;
    a.set_zones(vec![z]);
    b.load_state(&a.save_state()).unwrap();
    assert_eq!(b.zones()[0].gain_db, 0.0);
    assert_eq!(b.zones()[0].velocity_hi, 1.0);
}

#[test]
fn invalid_zone_state_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let mut b = kit(dir.path());
    let before = mapping(&b);
    let bad = br#"{"device":"daw.sampler","version":1,"params":{"gain":-20.0},"extra":{"zones":[{"root_key":300}]}}"#;
    assert!(b.load_state(bad).is_err());
    assert_eq!(mapping(&b), before);
    assert_eq!(b.param_value(GAIN), 0.0);
    // State saved before zones were stored keeps the current zones.
    let old = br#"{"device":"daw.sampler","version":1,"params":{"gain":-20.0}}"#;
    b.load_state(old).unwrap();
    assert_eq!(mapping(&b), before);
    assert_eq!(b.param_value(GAIN), -20.0);
}

/// Paths resolved against a project root are saved back as they were stored,
/// even when the resolver's buffer has no source or an absolute one.
#[test]
fn resolved_paths_are_saved_as_stored() {
    let dir = tempfile::tempdir().unwrap();
    write_hit(dir.path(), "kick.wav", 0.5);
    let mut a = Sampler::new();
    let kick = SampleBuffer::mono(vec![0.5; 10], 48_000.0).with_source("kick.wav");
    a.set_zones(vec![Zone::full(Arc::new(kick))]);
    let blob = a.save_state();

    let mut b = Sampler::new();
    b.load_state_with(&blob, |p| {
        SampleBuffer::load(dir.path().join(p)).ok().map(Arc::new)
    })
    .unwrap();
    assert_eq!(b.zones()[0].sample.len(), 4800);
    let mut c = Sampler::new();
    c.load_state_with(&b.save_state(), |_| {
        Some(Arc::new(SampleBuffer::mono(vec![0.1; 10], 48_000.0)))
    })
    .unwrap();
    for s in [&b, &c] {
        assert_eq!(
            s.zones()[0].source.as_deref(),
            Some(std::path::Path::new("kick.wav"))
        );
    }
    assert_eq!(mapping(&c), mapping(&a));

    // A buffer from a shared pool is used as is, not copied.
    let pooled = Arc::new(SampleBuffer::load(dir.path().join("kick.wav")).unwrap());
    let mut d = Sampler::new();
    d.load_state_with(&blob, |_| Some(pooled.clone())).unwrap();
    assert!(Arc::ptr_eq(&d.zones()[0].sample, &pooled));
    assert_eq!(mapping(&d), mapping(&a));
}

#[cfg(unix)]
#[test]
fn non_utf8_sample_path_does_not_break_saving() {
    use std::os::unix::ffi::OsStrExt;
    let path = std::path::Path::new(std::ffi::OsStr::from_bytes(b"/tmp/k\xffick.wav"));
    let mut a = Sampler::new();
    let kick = SampleBuffer::mono(vec![0.5; 10], 48_000.0).with_source(path);
    a.set_zones(vec![Zone::full(Arc::new(kick))]);
    let mut b = Sampler::new();
    b.load_state(&a.save_state()).unwrap();
    assert_eq!(b.zones().len(), 1);
}

/// Builds from before the zone map refuse state that has one, rather than
/// loading it and dropping the zones on the next save.
#[test]
fn zone_state_has_a_newer_version() {
    let blob = sampler_with(vec![0.5; 10], 48_000.0).save_state();
    let json: serde_json::Value = serde_json::from_slice(&blob).unwrap();
    assert_eq!(json["version"], 2);
    let other = daw_devices::create("daw.eq").unwrap().save_state();
    let json: serde_json::Value = serde_json::from_slice(&other).unwrap();
    assert_eq!(json["version"], 1);
}

/// Issue #27: Gain at its -inf minimum added to a positive zone gain in dB,
/// so the sampler wasn't silent.
#[test]
fn minimum_gain_is_silent_whatever_the_zone_gain() {
    let s = Setup::default();
    let mut sampler = Sampler::new();
    let mut zone = Zone::full(Arc::new(SampleBuffer::mono(vec![0.5; 4800], 48_000.0)));
    zone.gain_db = 12.0;
    sampler.set_zones(vec![zone]);
    sampler.set_param(GAIN, -70.0);
    let r = render(&mut sampler, 2000, &[], &[(0, note_on(60, 1.0))], s);
    assert!(r.channels[0].iter().all(|&x| x == 0.0));
}

/// Issue #27: a zone with sample rate 0 or NaN left voices stuck forever.
#[test]
fn invalid_sample_rate_plays_nothing() {
    let s = Setup::default();
    for rate in [0.0, f64::NAN, -48_000.0, f64::INFINITY] {
        let mut sampler = sampler_with(vec![0.5; 4800], rate);
        let r = render(&mut sampler, 2000, &[], &[(0, note_on(60, 1.0))], s);
        assert_eq!(sampler.active_voices(), 0, "{rate}");
        assert!(r.channels[0].iter().all(|&x| x == 0.0), "{rate}");
    }
}

/// Issue #27: All Sound Off (CC 120) only released voices, so a long release
/// kept sounding. It now fades them out within a few milliseconds.
#[test]
fn all_sound_off_silences_quickly() {
    let s = Setup::default();
    let mut sampler = sampler_with(vec![0.5; 480_000], s.sample_rate);
    sampler.set_param(RELEASE, 10_000.0);
    let events = [
        (0, note_on(60, 1.0)),
        (0, note_on(64, 1.0)),
        (1000, EventKind::Midi([0xB0, 120, 0])),
    ];
    let r = render(&mut sampler, 2000, &[], &events, s);
    assert_eq!(sampler.active_voices(), 0);
    assert!(r.channels[0][1000 + 200..].iter().all(|&x| x == 0.0));
    // A fade, not a cut: the first sample after the message is still loud.
    assert!(r.channels[0][1001] > 0.5);

    // All Notes Off (CC 123) still uses the release.
    let mut sampler = sampler_with(vec![0.5; 480_000], s.sample_rate);
    sampler.set_param(RELEASE, 10_000.0);
    let events = [
        (0, note_on(60, 1.0)),
        (1000, EventKind::Midi([0xB0, 123, 0])),
    ];
    let r = render(&mut sampler, 2000, &[], &events, s);
    assert!(r.channels[0][1999] > 0.4);
}
