mod common;

use common::{estimate_freq, max_abs_diff, test_signal};
use daw_media::{
    AudioData, Decoder, FlacBits, WavFormat, decode_file, load_file, resample, write_flac,
    write_wav,
};

fn data_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data")
}

fn roundtrip_wav(format: WavFormat, tolerance: f32) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.wav");
    let src = test_signal(48_000, 10_000, 2);
    write_wav(&path, &src, format).unwrap();
    let back = decode_file(&path).unwrap();
    assert_eq!(back.sample_rate, 48_000);
    assert_eq!(back.num_channels(), 2);
    assert_eq!(back.frames(), src.frames());
    for c in 0..2 {
        let d = max_abs_diff(&src.channels[c], &back.channels[c]);
        assert!(d <= tolerance, "{format:?} ch{c} diff {d}");
    }
}

#[test]
fn wav_float32_is_lossless() {
    roundtrip_wav(WavFormat::Float32, 0.0);
}

#[test]
fn wav_pcm24_roundtrip() {
    roundtrip_wav(WavFormat::Pcm24, 1.0 / 8_388_608.0 + 1e-7);
}

#[test]
fn wav_pcm16_roundtrip() {
    roundtrip_wav(WavFormat::Pcm16, 1.0 / 32_768.0 + 1e-6);
}

fn roundtrip_flac(bits: FlacBits, tolerance: f32) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.flac");
    let src = test_signal(44_100, 30_000, 2);
    write_flac(&path, &src, bits).unwrap();
    let back = decode_file(&path).unwrap();
    assert_eq!(back.sample_rate, 44_100);
    assert_eq!(back.frames(), src.frames());
    for c in 0..2 {
        let d = max_abs_diff(&src.channels[c], &back.channels[c]);
        assert!(d <= tolerance, "{bits:?} ch{c} diff {d}");
    }
}

#[test]
fn flac_24_roundtrip() {
    roundtrip_flac(FlacBits::B24, 1.0 / 8_388_608.0 + 1e-7);
}

#[test]
fn flac_shorter_than_one_block() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("short.flac");
    let src = test_signal(44_100, 1000, 1);
    write_flac(&path, &src, FlacBits::B24).unwrap();
    assert_eq!(decode_file(&path).unwrap().frames(), 1000);
}

#[test]
fn flac_16_roundtrip() {
    roundtrip_flac(FlacBits::B16, 1.0 / 32_768.0 + 1e-6);
}

#[test]
fn mono_and_many_channels_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    for ch in [1, 6] {
        let path = dir.path().join(format!("{ch}.wav"));
        let src = test_signal(48_000, 1000, ch);
        write_wav(&path, &src, WavFormat::Float32).unwrap();
        assert_eq!(decode_file(&path).unwrap(), src);
    }
}

#[test]
fn clipping_is_clamped_for_integer_formats() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("clip.wav");
    let src = AudioData {
        sample_rate: 48_000,
        channels: vec![vec![2.0, -2.0, 1.0, -1.0]],
    };
    write_wav(&path, &src, WavFormat::Pcm16).unwrap();
    let back = decode_file(&path).unwrap();
    assert!((back.channels[0][0] - 32767.0 / 32768.0).abs() < 1e-6);
    assert_eq!(back.channels[0][1], -1.0);
}

#[test]
fn decodes_ogg_vorbis_fixture() {
    let d = decode_file(data_dir().join("sine440.ogg")).unwrap();
    assert_eq!(d.sample_rate, 44_100);
    assert_eq!(d.num_channels(), 2);
    assert!((d.frames() as i64 - 22_050).abs() < 1200, "{} frames", d.frames());
    let f = estimate_freq(&d.channels[0], d.sample_rate);
    assert!((f - 440.0).abs() < 10.0, "freq {f}");
}

#[test]
fn decodes_mp3_fixture() {
    let d = decode_file(data_dir().join("sine440.mp3")).unwrap();
    assert_eq!(d.sample_rate, 44_100);
    assert_eq!(d.num_channels(), 2);
    assert!((d.frames() as i64 - 22_050).abs() < 2400, "{} frames", d.frames());
    let f = estimate_freq(&d.channels[1], d.sample_rate);
    assert!((f - 440.0).abs() < 10.0, "freq {f}");
}

#[test]
fn seek_is_sample_accurate_for_wav_and_flac() {
    let dir = tempfile::tempdir().unwrap();
    let src = test_signal(48_000, 50_000, 2);
    let wav = dir.path().join("s.wav");
    let flac = dir.path().join("s.flac");
    write_wav(&wav, &src, WavFormat::Float32).unwrap();
    write_flac(&flac, &src, FlacBits::B24).unwrap();
    for path in [&wav, &flac] {
        let mut dec = Decoder::open(path).unwrap();
        for target in [0u64, 1, 4095, 4096, 12_345, 33_333, 49_000] {
            dec.seek(target).unwrap();
            let mut buf = vec![0.0f32; 2 * 500];
            let n = dec.read(&mut buf).unwrap();
            assert_eq!(n, 500.min(50_000 - target as usize));
            for i in 0..n {
                let want = src.channels[0][target as usize + i];
                assert!(
                    (buf[i * 2] - want).abs() < 1e-5,
                    "{} seek {target} frame {i}: {} vs {want}",
                    path.display(),
                    buf[i * 2]
                );
            }
        }
        // Past the end reads nothing.
        dec.seek(60_000).unwrap();
        let mut buf = vec![0.0f32; 64];
        assert_eq!(dec.read(&mut buf).unwrap(), 0);
    }
}

#[test]
fn resample_preserves_pitch_and_length() {
    let src = test_signal(44_100, 44_100, 2);
    let out = resample(&src, 48_000).unwrap();
    assert_eq!(out.sample_rate, 48_000);
    assert!((out.frames() as i64 - 48_000).abs() <= 1, "{}", out.frames());
    for c in 0..2 {
        let want = 220.0 * (c + 1) as f32 + 0.37;
        let f = estimate_freq(&out.channels[c][1000..47_000], 48_000);
        assert!((f - want).abs() < 2.0, "ch{c}: {f} vs {want}");
    }
    // The startup delay is trimmed: output should be in phase with the input.
    let t = 10_000usize;
    let expected = 0.8 * (std::f32::consts::TAU * 220.37 * t as f32 / 48_000.0).sin();
    assert!((out.channels[0][t] - expected).abs() < 0.01);
}

#[test]
fn load_file_resamples_to_engine_rate() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("r.wav");
    write_wav(&path, &test_signal(96_000, 9600, 1), WavFormat::Float32).unwrap();
    let d = load_file(&path, 48_000).unwrap();
    assert_eq!(d.sample_rate, 48_000);
    assert!((d.frames() as i64 - 4800).abs() <= 1);
}

#[test]
fn missing_and_garbage_files_are_errors() {
    let dir = tempfile::tempdir().unwrap();
    assert!(decode_file(dir.path().join("nope.wav")).is_err());
    let junk = dir.path().join("junk.wav");
    std::fs::write(&junk, b"definitely not audio").unwrap();
    assert!(decode_file(&junk).is_err());
}
