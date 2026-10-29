#![allow(dead_code)]

use daw_media::AudioData;

/// A stereo test signal: different sines per channel so swapped channels show up.
pub fn test_signal(rate: u32, frames: usize, channels: usize) -> AudioData {
    let channels = (0..channels)
        .map(|c| {
            let freq = 220.0 * (c + 1) as f32 + 0.37;
            (0..frames)
                .map(|i| 0.8 * (std::f32::consts::TAU * freq * i as f32 / rate as f32).sin())
                .collect()
        })
        .collect();
    AudioData {
        sample_rate: rate,
        channels,
    }
}

pub fn max_abs_diff(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len(), "length mismatch");
    a.iter()
        .zip(b)
        .map(|(x, y)| (x - y).abs())
        .fold(0.0, f32::max)
}

/// Estimate frequency from upward zero crossings.
pub fn estimate_freq(x: &[f32], rate: u32) -> f32 {
    let crossings = x.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
    crossings as f32 * rate as f32 / x.len() as f32
}
