use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use daw_core::atomic_file;

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
            WavFormat::Pcm16 => w.write_sample(to_int(s, 16, 0.0) as i16),
            WavFormat::Pcm24 => w.write_sample(to_int(s, 24, 0.0)),
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

/// The most audio a WAV file holds. RIFF stores sizes as `u32` and hound does not
/// check them. The margin covers the header and keeps the RIFF size clear of
/// `u32::MAX`, which readers take to mean "unknown length".
pub(crate) const WAV_MAX_DATA_BYTES: u64 = u32::MAX as u64 - (1 << 20);

/// `s` as a `bits`-bit integer, with `noise` LSBs (0 for none) added before rounding.
fn to_int(s: f32, bits: u32, noise: f32) -> i32 {
    let max = ((1i64 << (bits - 1)) - 1) as f32;
    let min = -((1i64 << (bits - 1)) as f32);
    (s * (max + 1.0) + noise).round().clamp(min, max) as i32
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

/// A file being written under a temporary name and moved into place by
/// [`commit`](Self::commit), through [`daw_core::atomic_file`]. Dropped without
/// committing, the temporary file is removed, so a failure part way (disk full, a
/// cancelled export) leaves any existing file untouched and no partial file behind.
struct Rewrite {
    /// What the caller asked for, for removing its peak cache.
    path: PathBuf,
    file: atomic_file::Replacement,
}

impl Rewrite {
    /// A symlink is followed and its target replaced; an existing file keeps its
    /// permissions, and a read-only one is refused.
    fn begin(path: &Path) -> Result<(Self, File)> {
        let link = std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink());
        let target = if link {
            std::fs::canonicalize(path).map_err(|e| MediaError::io(path, e))?
        } else {
            path.to_owned()
        };
        let permissions = match std::fs::metadata(&target) {
            Ok(m) if m.permissions().readonly() => {
                return Err(MediaError::io(
                    &target,
                    std::io::Error::new(std::io::ErrorKind::PermissionDenied, "file is read-only"),
                ));
            }
            Ok(m) => Some(m.permissions()),
            Err(_) => None,
        };
        let (replacement, file) =
            atomic_file::Replacement::begin(&target).map_err(|e| MediaError::io(&target, e))?;
        if let Some(p) = permissions {
            replacement
                .restrict(&file, p)
                .map_err(|e| MediaError::io(replacement.tmp(), e))?;
        }
        let this = Self {
            path: path.to_owned(),
            file: replacement,
        };
        Ok((this, file))
    }

    /// The temporary file's path, for error messages.
    fn tmp(&self) -> &Path {
        self.file.tmp()
    }

    /// Flushes `file` (the one [`begin`](Self::begin) returned, fully written) to disk
    /// and renames it into place. A stale peak cache for the old contents is removed.
    fn commit(self, file: File) -> Result<()> {
        let target = self.file.target().to_owned();
        self.file
            .commit(file)
            .map_err(|e| MediaError::io(&target, e))?;
        // Same length within the mtime granularity would otherwise look current. The new
        // file is in place either way, so a failed delete is not an error.
        for p in [&self.path, &target] {
            let _ = std::fs::remove_file(PeakCache::path_for(p));
        }
        Ok(())
    }
}

/// Write `data` as a WAV file. An existing file is only replaced once the new one is
/// complete.
pub fn write_wav(path: impl AsRef<Path>, data: &AudioData, format: WavFormat) -> Result<()> {
    write_whole(path.as_ref(), data, FileFormat::Wav(format))
}

/// Write `data` as a FLAC file. An existing file is only replaced once the new one is
/// complete.
pub fn write_flac(path: impl AsRef<Path>, data: &AudioData, bits: FlacBits) -> Result<()> {
    write_whole(path.as_ref(), data, FileFormat::Flac(bits))
}

/// Writes all of `data` through an [`AudioWriter`], without dither.
fn write_whole(path: &Path, data: &AudioData, format: FileFormat) -> Result<()> {
    check(data)?;
    let mut writer =
        AudioWriter::create(path, format, data.num_channels(), data.sample_rate, false)?;
    let planes: Vec<&[f32]> = data.channels.iter().map(Vec::as_slice).collect();
    writer.write(&planes)?;
    writer.finish()
}

/// The kind of file an [`AudioWriter`] makes.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum FileFormat {
    Wav(WavFormat),
    Flac(FlacBits),
}

impl FileFormat {
    /// Bits per sample of an integer format; `None` for float.
    pub fn int_bits(self) -> Option<u32> {
        match self {
            Self::Wav(WavFormat::Pcm16) | Self::Flac(FlacBits::B16) => Some(16),
            Self::Wav(WavFormat::Pcm24) | Self::Flac(FlacBits::B24) => Some(24),
            Self::Wav(WavFormat::Float32) => None,
        }
    }
}

/// Triangular (TPDF) dither of one LSB peak, added before rounding to an integer
/// format so quiet passages fade into noise instead of distortion. Seeded, so the
/// same audio always makes the same file.
#[derive(Clone, Debug)]
struct Dither(u64);

impl Dither {
    fn uniform(&mut self) -> f32 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        let x = self.0.wrapping_mul(0x2545_f491_4f6c_dd1d);
        (x >> 40) as f32 / (1u64 << 24) as f32 - 0.5
    }

    /// Noise in LSBs, in `(-1, 1)`.
    fn next(&mut self) -> f32 {
        self.uniform() + self.uniform()
    }
}

enum Sink {
    Wav {
        writer: hound::WavWriter<BufWriter<File>>,
        format: WavFormat,
        /// Bytes of audio written so far, against the 4 GiB RIFF limit.
        bytes: u64,
    },
    Flac(Box<FlacSink>),
}

/// A FLAC stream encoded a frame at a time, so a long export never sits in memory.
/// The STREAMINFO header goes out first as a placeholder and is rewritten with the
/// totals and MD5 at the end; it has a fixed size, so nothing moves.
struct FlacSink {
    out: BufWriter<File>,
    config: flacenc::error::Verified<flacenc::config::Encoder>,
    info: flacenc::component::StreamInfo,
    framebuf: flacenc::source::FrameBuf,
    context: flacenc::source::Context,
    /// Interleaved samples of the frame being filled.
    pending: Vec<i32>,
    block: usize,
    channels: usize,
    bits: u32,
    sink: flacenc::bitsink::ByteSink,
}

impl FlacSink {
    fn new(out: File, channels: usize, bits: u32, rate: u32) -> Result<Self> {
        let encode = |e: &dyn std::fmt::Debug| MediaError::Encode(format!("{e:?}"));
        let config = flacenc::config::Encoder::default()
            .into_verified()
            .map_err(|(_, e)| encode(&e))?;
        let block = config.block_size;
        let info = flacenc::component::StreamInfo::new(rate as usize, channels, bits as usize)
            .map_err(|e| encode(&e))?;
        let framebuf =
            flacenc::source::FrameBuf::with_size(channels, block).map_err(|e| encode(&e))?;
        let mut this = Self {
            out: BufWriter::new(out),
            config,
            info,
            framebuf,
            context: flacenc::source::Context::new(bits as usize, channels),
            pending: Vec::with_capacity(block * channels),
            block,
            channels,
            bits,
            sink: flacenc::bitsink::ByteSink::new(),
        };
        this.write_header()?;
        Ok(this)
    }

    fn io(e: std::io::Error) -> MediaError {
        MediaError::Encode(format!("writing FLAC: {e}"))
    }

    /// "fLaC" and STREAMINFO as they stand, at the current position.
    fn write_header(&mut self) -> Result<()> {
        let header = flacenc::component::Stream::with_stream_info(self.info.clone());
        self.sink.clear();
        header
            .write(&mut self.sink)
            .map_err(|e| MediaError::Encode(format!("{e:?}")))?;
        self.out.write_all(self.sink.as_slice()).map_err(Self::io)
    }

    fn push(&mut self, sample: i32) -> Result<()> {
        self.pending.push(sample);
        if self.pending.len() == self.block * self.channels {
            self.encode_pending()?;
        }
        Ok(())
    }

    fn encode_pending(&mut self) -> Result<()> {
        use flacenc::source::Fill;
        if self.pending.is_empty() {
            return Ok(());
        }
        let encode = |e: &dyn std::fmt::Debug| MediaError::Encode(format!("{e:?}"));
        self.framebuf
            .fill_interleaved(&self.pending)
            .map_err(|e| encode(&e))?;
        self.context
            .fill_interleaved(&self.pending)
            .map_err(|e| encode(&e))?;
        let number = self.context.current_frame_number().unwrap_or(0);
        let frame =
            flacenc::encode_fixed_size_frame(&self.config, &self.framebuf, number, &self.info)
                .map_err(|e| encode(&e))?;
        self.info.update_frame_info(&frame);
        self.sink.clear();
        frame.write(&mut self.sink).map_err(|e| encode(&e))?;
        self.out.write_all(self.sink.as_slice()).map_err(Self::io)?;
        self.pending.clear();
        Ok(())
    }

    /// Encodes the short last frame and rewrites the header with the totals.
    fn finish(mut self) -> Result<()> {
        self.encode_pending()?;
        let encode = |e: &dyn std::fmt::Debug| MediaError::Encode(format!("{e:?}"));
        // The minimum excludes the last block, and Symphonia refuses streams whose
        // minimum is the short last block, so say min == max as libFLAC does for a
        // fixed-size stream.
        self.info
            .set_block_sizes(self.block, self.block)
            .map_err(|e| encode(&e))?;
        self.info.set_md5_digest(&self.context.md5_digest());
        if self.info.total_samples() == 0 {
            // No frames: the frame sizes were never set.
            self.info = {
                let mut empty = flacenc::component::StreamInfo::new(
                    self.info.sample_rate(),
                    self.channels,
                    self.bits as usize,
                )
                .map_err(|e| encode(&e))?;
                empty
                    .set_block_sizes(self.block, self.block)
                    .map_err(|e| encode(&e))?;
                empty.set_md5_digest(&self.context.md5_digest());
                empty
            };
        }
        use std::io::Seek;
        self.out
            .seek(std::io::SeekFrom::Start(0))
            .map_err(Self::io)?;
        self.write_header()?;
        self.out.flush().map_err(Self::io)
    }
}

/// Writes an audio file a block at a time, for exports too long to hold as
/// [`AudioData`]. The file appears at its path only when [`finish`](Self::finish)
/// succeeds; dropping the writer before that (an error, a cancelled export) leaves no
/// file behind and any existing one untouched.
pub struct AudioWriter {
    rewrite: Rewrite,
    /// The file the rewrite commits; `sink` writes through a clone of it.
    file: File,
    sink: Sink,
    channels: usize,
    dither: Option<Dither>,
    frames: u64,
}

impl AudioWriter {
    /// Starts a file of `channels` channels at `sample_rate`. `dither` adds TPDF dither
    /// when rounding to an integer format; it does nothing for float WAV.
    pub fn create(
        path: impl AsRef<Path>,
        format: FileFormat,
        channels: usize,
        sample_rate: u32,
        dither: bool,
    ) -> Result<Self> {
        if channels == 0 || channels > u16::MAX as usize {
            return Err(MediaError::Encode(format!("bad channel count {channels}")));
        }
        if sample_rate == 0 {
            return Err(MediaError::Encode("sample rate is 0".into()));
        }
        let (rewrite, file) = Rewrite::begin(path.as_ref())?;
        let sink = match format {
            FileFormat::Wav(format) => {
                let clone = file
                    .try_clone()
                    .map_err(|e| MediaError::io(rewrite.tmp(), e))?;
                let writer = hound::WavWriter::new(
                    BufWriter::new(clone),
                    format.spec(channels, sample_rate),
                )?;
                Sink::Wav {
                    writer,
                    format,
                    bytes: 0,
                }
            }
            FileFormat::Flac(bits) => {
                let clone = file
                    .try_clone()
                    .map_err(|e| MediaError::io(rewrite.tmp(), e))?;
                let bits = match bits {
                    FlacBits::B16 => 16,
                    FlacBits::B24 => 24,
                };
                Sink::Flac(Box::new(FlacSink::new(clone, channels, bits, sample_rate)?))
            }
        };
        let dither =
            (dither && format.int_bits().is_some()).then_some(Dither(0x9e37_79b9_7f4a_7c15));
        Ok(Self {
            rewrite,
            file,
            sink,
            channels,
            dither,
            frames: 0,
        })
    }

    /// Frames written so far.
    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// Appends one block: a slice per channel, all the same length.
    pub fn write(&mut self, planes: &[&[f32]]) -> Result<()> {
        if planes.len() != self.channels {
            return Err(MediaError::Encode(format!(
                "{} channels given to a {}-channel file",
                planes.len(),
                self.channels
            )));
        }
        let n = planes.first().map_or(0, |p| p.len());
        if planes.iter().any(|p| p.len() != n) {
            return Err(MediaError::Encode("channels differ in length".into()));
        }
        let tmp = self.rewrite.tmp();
        match &mut self.sink {
            Sink::Wav {
                writer,
                format,
                bytes,
            } => {
                let size = u64::from(format.spec(1, 1).bits_per_sample / 8);
                *bytes += n as u64 * self.channels as u64 * size;
                if *bytes > WAV_MAX_DATA_BYTES {
                    return Err(MediaError::Encode(format!(
                        "{bytes} bytes of audio do not fit in a WAV file (4 GiB limit)"
                    )));
                }
                let bits = match format {
                    WavFormat::Pcm16 => 16,
                    WavFormat::Pcm24 => 24,
                    WavFormat::Float32 => 0,
                };
                for i in 0..n {
                    for plane in planes {
                        let r = match (&mut self.dither, bits) {
                            (Some(d), 16) => {
                                writer.write_sample(to_int(plane[i], 16, d.next()) as i16)
                            }
                            (Some(d), 24) => writer.write_sample(to_int(plane[i], 24, d.next())),
                            _ => format.write_sample(writer, plane[i]),
                        };
                        r.map_err(|e| wav_error(tmp, e))?;
                    }
                }
            }
            Sink::Flac(flac) => {
                for i in 0..n {
                    for plane in planes {
                        let noise = self.dither.as_mut().map_or(0.0, Dither::next);
                        flac.push(to_int(plane[i], flac.bits, noise))?;
                    }
                }
            }
        }
        self.frames += n as u64;
        Ok(())
    }

    /// Completes the file and moves it into place.
    pub fn finish(self) -> Result<()> {
        let Self {
            rewrite,
            file,
            sink,
            ..
        } = self;
        match sink {
            Sink::Wav { writer, .. } => {
                writer.finalize().map_err(|e| wav_error(rewrite.tmp(), e))?
            }
            Sink::Flac(flac) => flac.finish()?,
        }
        rewrite.commit(file)
    }
}

/// A hound error, with I/O errors (a full disk) kept as such.
fn wav_error(path: &Path, e: hound::Error) -> MediaError {
    match e {
        hound::Error::IoError(e) => MediaError::io(path, e),
        e => e.into(),
    }
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
        // More than a WAV file holds: refused part way, as a full disk would.
        let mut w = AudioWriter::create(&path, FileFormat::Wav(WavFormat::Pcm16), 1, 48_000, false)
            .unwrap();
        w.write(&[&[0.0; 4096]]).unwrap();
        let Sink::Wav { bytes, .. } = &mut w.sink else {
            unreachable!()
        };
        *bytes = WAV_MAX_DATA_BYTES;
        assert!(w.write(&[&[0.0; 4096]]).is_err());
        drop(w);
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

    fn names(dir: &Path) -> Vec<std::ffi::OsString> {
        let mut names: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        names.sort();
        names
    }

    /// A writer's file appears only when it is finished; one dropped part way (a
    /// cancelled export) leaves nothing, not even its temporary file.
    #[test]
    fn audio_writer_file_appears_only_when_finished() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mix.flac");
        let l = [0.25f32; 100];
        let r = [-0.5f32; 100];
        for format in [
            FileFormat::Flac(FlacBits::B16),
            FileFormat::Wav(WavFormat::Pcm24),
        ] {
            let mut w = AudioWriter::create(&path, format, 2, 48_000, false).unwrap();
            w.write(&[&l, &r]).unwrap();
            assert!(!path.exists());
            drop(w);
            assert!(names(dir.path()).is_empty(), "{format:?}");
        }

        let mut w =
            AudioWriter::create(&path, FileFormat::Flac(FlacBits::B24), 2, 44_100, false).unwrap();
        assert!(w.write(&[&l]).is_err(), "wrong channel count");
        assert!(w.write(&[&l, &r[..50]]).is_err(), "uneven channels");
        w.write(&[&l, &r]).unwrap();
        w.write(&[&l[..10], &r[..10]]).unwrap();
        assert_eq!(w.frames(), 110);
        w.finish().unwrap();
        assert_eq!(names(dir.path()), [std::ffi::OsString::from("mix.flac")]);
        let data = crate::decode_file(&path).unwrap();
        assert_eq!((data.sample_rate, data.frames()), (44_100, 110));
        assert!(data.channels[0].iter().all(|&x| (x - 0.25).abs() < 1e-6));
        assert!(data.channels[1].iter().all(|&x| (x + 0.5).abs() < 1e-6));
    }

    /// Dither stays within a couple of LSBs, is the same every time, and actually
    /// varies the samples (a quiet DC level would otherwise round to one value).
    #[test]
    fn dither_is_small_repeatable_noise() {
        let dir = tempfile::tempdir().unwrap();
        let level = 0.3 / 32_768.0;
        let input = vec![level; 4096];
        let write = |name: &str, dither: bool| {
            let path = dir.path().join(name);
            let mut w =
                AudioWriter::create(&path, FileFormat::Wav(WavFormat::Pcm16), 1, 48_000, dither)
                    .unwrap();
            w.write(&[&input]).unwrap();
            w.finish().unwrap();
            path
        };
        let plain = crate::decode_file(write("plain.wav", false)).unwrap();
        assert!(
            plain.channels[0].iter().all(|&x| x == 0.0),
            "rounds to silence"
        );
        let a = write("a.wav", true);
        let b = write("b.wav", true);
        assert_eq!(std::fs::read(&a).unwrap(), std::fs::read(&b).unwrap());
        let dithered = crate::decode_file(&a).unwrap();
        let lsb = 1.0 / 32_768.0;
        assert!(
            dithered.channels[0]
                .iter()
                .all(|&x| (x - level).abs() <= 1.5 * lsb)
        );
        assert!(dithered.channels[0].iter().any(|&x| x != 0.0));
        let mean = dithered.channels[0].iter().sum::<f32>() / 4096.0;
        assert!((mean - level).abs() < 0.2 * lsb, "keeps the level: {mean}");
    }
}
