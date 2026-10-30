use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use flacenc::component::BitRepr;
use flacenc::error::Verify;

use crate::{AudioData, MediaError, PeakCache, Result};

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

/// Write the file at `path` through a temp file next to it, then rename it into place,
/// so a failure part way (disk full) leaves any existing file untouched. A symlink is
/// followed and its target replaced; an existing file keeps its permissions, and a
/// read-only one is refused. A stale peak cache for the old contents is removed.
fn replace_file(path: &Path, write: impl FnOnce(&mut BufWriter<File>) -> Result<()>) -> Result<()> {
    let link = std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink());
    let target = if link {
        std::fs::canonicalize(path).map_err(|e| MediaError::io(path, e))?
    } else {
        path.to_owned()
    };
    let old_permissions = match std::fs::metadata(&target) {
        Ok(m) if m.permissions().readonly() => {
            return Err(MediaError::io(
                &target,
                std::io::Error::new(std::io::ErrorKind::PermissionDenied, "file is read-only"),
            ));
        }
        Ok(m) => Some(m.permissions()),
        Err(_) => None,
    };
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    // Short and fixed-length, so a long file name can't push it past NAME_MAX.
    let tmp: PathBuf = target.with_file_name(format!(".kadence-{}-{n}.tmp", std::process::id()));
    let result = (|| {
        let file = File::create_new(&tmp).map_err(|e| MediaError::io(&tmp, e))?;
        let mut w = BufWriter::new(file);
        write(&mut w)?;
        let file = w
            .into_inner()
            .map_err(|e| MediaError::io(&tmp, e.into_error()))?;
        if let Some(p) = old_permissions {
            file.set_permissions(p)
                .map_err(|e| MediaError::io(&tmp, e))?;
        }
        file.sync_all().map_err(|e| MediaError::io(&tmp, e))?;
        std::fs::rename(&tmp, &target).map_err(|e| MediaError::io(&target, e))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
        return result;
    }
    // Same length within the mtime granularity would otherwise look current. The new
    // file is in place either way, so a failed delete is not an error.
    for p in [path, target.as_path()] {
        let _ = std::fs::remove_file(PeakCache::path_for(p));
    }
    Ok(())
}

/// Write `data` as a WAV file. An existing file is only replaced once the new one is
/// complete.
pub fn write_wav(path: impl AsRef<Path>, data: &AudioData, format: WavFormat) -> Result<()> {
    check(data)?;
    let spec = format.spec(data.num_channels(), data.sample_rate);
    let bytes = data.frames() as u64 * data.num_channels() as u64 * spec.bits_per_sample as u64 / 8;
    // RIFF sizes are u32 (and hound does not check them); leave room for the header.
    if bytes > u32::MAX as u64 - (1 << 20) {
        return Err(MediaError::Encode(format!(
            "{bytes} bytes of audio do not fit in a WAV file (4 GiB limit)"
        )));
    }
    replace_file(path.as_ref(), |out| {
        let mut w = hound::WavWriter::new(out, spec)?;
        for i in 0..data.frames() {
            for plane in &data.channels {
                format.write_sample(&mut w, plane[i])?;
            }
        }
        w.finalize()?;
        Ok(())
    })
}

/// Write `data` as a FLAC file. An existing file is only replaced once the new one is
/// complete.
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
    replace_file(path, |out| {
        out.write_all(&bytes).map_err(|e| MediaError::io(path, e))
    })
}

#[cfg(test)]
mod replace_tests {
    use super::*;

    /// Issue #28: writing over a file in place destroyed the original when the write
    /// failed part way (disk full during a bounce).
    #[test]
    fn failed_write_leaves_the_original_and_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mix.wav");
        let original = AudioData {
            sample_rate: 48_000,
            channels: vec![vec![0.5; 1000]],
        };
        write_wav(&path, &original, WavFormat::Pcm16).unwrap();
        let before = std::fs::read(&path).unwrap();
        let err = replace_file(&path, |out| {
            out.write_all(&[0; 4096]).unwrap();
            Err(MediaError::Encode("No space left on device".into()))
        });
        assert!(err.is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, [std::ffi::OsString::from("mix.wav")]);
    }

    /// Writing through the temp file must not turn a symlink into a plain file, drop
    /// the file's permissions, or overwrite a read-only file.
    #[cfg(unix)]
    #[test]
    fn rewrite_keeps_symlinks_and_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real.wav");
        let link = dir.path().join("link.wav");
        let one = AudioData {
            sample_rate: 48_000,
            channels: vec![vec![0.5; 100]],
        };
        let two = AudioData {
            sample_rate: 48_000,
            channels: vec![vec![0.25; 200]],
        };
        write_wav(&real, &one, WavFormat::Pcm16).unwrap();
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o640)).unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        write_flac(&link, &two, FlacBits::B16).unwrap();
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(crate::decode_file(&real).unwrap().frames(), 200);
        let mode = std::fs::metadata(&real).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o640);

        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o440)).unwrap();
        assert!(write_wav(&real, &one, WavFormat::Pcm16).is_err());
        assert_eq!(crate::decode_file(&real).unwrap().frames(), 200);
    }
}
