use std::path::Path;

use flacenc::component::BitRepr;
use flacenc::error::Verify;

use crate::{AudioData, MediaError, Result};

/// Sample encoding for WAV files.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum WavFormat {
    Pcm16,
    Pcm24,
    #[default]
    Float32,
}

impl WavFormat {
    pub(crate) fn spec(self, channels: usize, sample_rate: u32) -> hound::WavSpec {
        let (bits_per_sample, sample_format) = match self {
            WavFormat::Pcm16 => (16, hound::SampleFormat::Int),
            WavFormat::Pcm24 => (24, hound::SampleFormat::Int),
            WavFormat::Float32 => (32, hound::SampleFormat::Float),
        };
        hound::WavSpec {
            channels: channels as u16,
            sample_rate,
            bits_per_sample,
            sample_format,
        }
    }

    /// Write one sample. Integer formats are clamped and rounded (no dither).
    pub(crate) fn write_sample<W: std::io::Write + std::io::Seek>(
        self,
        w: &mut hound::WavWriter<W>,
        s: f32,
    ) -> hound::Result<()> {
        match self {
            WavFormat::Pcm16 => w.write_sample(to_int(s, 16) as i16),
            WavFormat::Pcm24 => w.write_sample(to_int(s, 24)),
            WavFormat::Float32 => w.write_sample(s),
        }
    }
}

/// Bit depth for FLAC files.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum FlacBits {
    B16,
    #[default]
    B24,
}

fn to_int(s: f32, bits: u32) -> i32 {
    let max = ((1i64 << (bits - 1)) - 1) as f32;
    let min = -((1i64 << (bits - 1)) as f32);
    (s * (max + 1.0)).round().clamp(min, max) as i32
}

fn check(data: &AudioData) -> Result<()> {
    let ch = data.num_channels();
    if ch == 0 || ch > u16::MAX as usize {
        return Err(MediaError::Encode(format!("bad channel count {ch}")));
    }
    if data.sample_rate == 0 {
        return Err(MediaError::Encode("sample rate is 0".into()));
    }
    if data.channels.iter().any(|c| c.len() != data.frames()) {
        return Err(MediaError::Encode("channels differ in length".into()));
    }
    Ok(())
}

/// Write `data` as a WAV file.
pub fn write_wav(path: impl AsRef<Path>, data: &AudioData, format: WavFormat) -> Result<()> {
    check(data)?;
    let spec = format.spec(data.num_channels(), data.sample_rate);
    let mut w = hound::WavWriter::create(path, spec)?;
    for i in 0..data.frames() {
        for plane in &data.channels {
            format.write_sample(&mut w, plane[i])?;
        }
    }
    w.finalize()?;
    Ok(())
}

/// Write `data` as a FLAC file.
pub fn write_flac(path: impl AsRef<Path>, data: &AudioData, bits: FlacBits) -> Result<()> {
    check(data)?;
    let path = path.as_ref();
    let bits = match bits {
        FlacBits::B16 => 16,
        FlacBits::B24 => 24,
    };
    let ch = data.num_channels();
    let mut samples = Vec::with_capacity(data.frames() * ch);
    for i in 0..data.frames() {
        for plane in &data.channels {
            samples.push(to_int(plane[i], bits as u32));
        }
    }
    let config = flacenc::config::Encoder::default()
        .into_verified()
        .map_err(|(_, e)| MediaError::Encode(format!("{e:?}")))?;
    let source =
        flacenc::source::MemSource::from_samples(&samples, ch, bits, data.sample_rate as usize);
    let stream = flacenc::encode_with_fixed_block_size(&config, source, config.block_size)
        .map_err(|e| MediaError::Encode(format!("{e:?}")))?;
    let mut sink = flacenc::bitsink::ByteSink::new();
    stream
        .write(&mut sink)
        .map_err(|e| MediaError::Encode(format!("{e:?}")))?;
    let mut bytes = sink.as_slice().to_vec();
    // flacenc records the short final block as the minimum block size. The spec says the
    // minimum excludes the last block, and Symphonia refuses such streams, so make the
    // fixed-size stream say min == max as libFLAC does. STREAMINFO follows "fLaC" and
    // its 4-byte block header, so min is at bytes 8..10 and max at 10..12.
    if bytes.len() >= 12 && &bytes[..4] == b"fLaC" {
        let (min, max) = bytes[8..12].split_at_mut(2);
        min.copy_from_slice(max);
    }
    std::fs::write(path, bytes).map_err(|e| MediaError::io(path, e))
}
