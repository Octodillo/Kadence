use std::fs::File;
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};

use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::{Time, TimeBase, Timestamp};

use crate::{AudioData, MediaError, Result, resample};

/// Basic facts about an audio file.
#[derive(Clone, Debug, PartialEq)]
pub struct AudioFileInfo {
    pub path: PathBuf,
    pub sample_rate: u32,
    pub channels: usize,
    /// Total frames if the container says so. Some MP3 and OGG files don't.
    pub frames: Option<u64>,
    /// Short format name from the demuxer, e.g. "wave", "flac", "ogg", "mp3".
    pub format: &'static str,
}

/// A streaming decoder for one audio file (WAV, AIFF, FLAC, OGG Vorbis, MP3).
///
/// Produces interleaved `f32` at the file's own rate. Not real-time safe: use it from
/// a normal thread (the [`DiskStreamer`](crate::DiskStreamer) does).
pub struct Decoder {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    track_id: u32,
    time_base: Option<TimeBase>,
    info: AudioFileInfo,
    /// Interleaved samples decoded but not yet handed out.
    pending: Vec<f32>,
    pending_pos: usize,
    /// Scratch for channel remapping when a packet's layout differs from `info`.
    scratch: Vec<f32>,
    /// Frames still to drop after an accurate seek landed early.
    skip: u64,
    position: u64,
    eof: bool,
}

/// The sample rates a file may state. A header can hold any 32-bit number,
/// and everything sized from the rate (resampler filters above all) would
/// follow it.
pub const SAMPLE_RATES: RangeInclusive<u32> = 1_000..=768_000;

/// The most channels a file may state. Buffers are sized per channel, and a
/// header can claim tens of thousands.
pub const MAX_CHANNELS: usize = 64;

/// Refuses a sample rate outside [`SAMPLE_RATES`].
pub(crate) fn check_rate(rate: u32) -> Result<()> {
    if SAMPLE_RATES.contains(&rate) {
        Ok(())
    } else {
        Err(MediaError::Unsupported(format!("sample rate of {rate} Hz")))
    }
}

/// Refuses more than [`MAX_CHANNELS`] channels.
pub(crate) fn check_channels(channels: usize) -> Result<()> {
    if channels <= MAX_CHANNELS {
        Ok(())
    } else {
        Err(MediaError::Unsupported(format!("{channels} channels")))
    }
}

/// Opens `path` for reading if it is a regular file. A FIFO or a device can
/// block for good, on opening or on reading, so those are refused; the check
/// is on the open file, which cannot be swapped for another afterwards.
fn open_regular(path: &Path) -> Result<File> {
    let mut options = File::options();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Without this, opening a FIFO waits for a writer.
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(|e| MediaError::io(path, e))?;
    let meta = file.metadata().map_err(|e| MediaError::io(path, e))?;
    if !meta.is_file() {
        return Err(MediaError::Unsupported("not a regular file".into()));
    }
    Ok(file)
}

impl Decoder {
    /// Opens an audio file. The file is untrusted: its header is checked
    /// here, so nothing later sizes work from a value no real file has.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let file = open_regular(path)?;
        let mss = MediaSourceStream::new(Box::new(file), Default::default());
        let mut hint = Hint::new();
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            hint.with_extension(ext);
        }
        let format = symphonia::default::get_probe().probe(
            &hint,
            mss,
            FormatOptions::default(),
            MetadataOptions::default(),
        )?;
        let track = format
            .default_track(TrackType::Audio)
            .ok_or_else(|| MediaError::Unsupported("no audio track".into()))?;
        let params = track
            .codec_params
            .as_ref()
            .and_then(|p| p.audio())
            .ok_or_else(|| MediaError::Unsupported("no audio codec parameters".into()))?;
        let decoder = symphonia::default::get_codecs()
            .make_audio_decoder(params, &AudioDecoderOptions::default())?;
        let sample_rate = params
            .sample_rate
            .ok_or_else(|| MediaError::Unsupported("unknown sample rate".into()))?;
        check_rate(sample_rate)?;
        let channels = params.channels.as_ref().map_or(0, |c| c.count());
        check_channels(channels)?;
        let track_id = track.id;
        let time_base = track.time_base;
        let frames = track.num_frames;
        let short_name = format.format_info().short_name;
        let mut this = Self {
            format,
            decoder,
            track_id,
            time_base,
            info: AudioFileInfo {
                path: path.to_owned(),
                sample_rate,
                channels,
                frames,
                format: short_name,
            },
            pending: Vec::new(),
            pending_pos: 0,
            scratch: Vec::new(),
            skip: 0,
            position: 0,
            eof: false,
        };
        if this.info.channels == 0 {
            // Some codecs only reveal the layout with the first packet.
            this.decode_next()?;
            this.info.channels = this.decoder.last_decoded().spec().channels().count();
            if this.info.channels == 0 {
                return Err(MediaError::Unsupported("unknown channel count".into()));
            }
            check_channels(this.info.channels)?;
        }
        Ok(this)
    }

    pub fn info(&self) -> &AudioFileInfo {
        &self.info
    }

    /// Frame index of the next frame [`read`](Self::read) will return.
    pub fn position(&self) -> u64 {
        self.position
    }

    /// Fill `out` with interleaved frames. Returns the number of frames written; fewer than
    /// requested only at end of file, and 0 once the file is exhausted.
    pub fn read(&mut self, out: &mut [f32]) -> Result<usize> {
        let ch = self.info.channels;
        let want = out.len() / ch;
        let mut done = 0;
        while done < want {
            if self.pending_pos >= self.pending.len() {
                if self.eof || !self.decode_next()? {
                    break;
                }
                continue;
            }
            let avail = (self.pending.len() - self.pending_pos) / ch;
            let n = avail.min(want - done);
            out[done * ch..(done + n) * ch]
                .copy_from_slice(&self.pending[self.pending_pos..self.pending_pos + n * ch]);
            self.pending_pos += n * ch;
            done += n;
        }
        self.position += done as u64;
        Ok(done)
    }

    /// Move to `frame`. The next [`read`](Self::read) starts exactly there for formats with
    /// sample-accurate seeking (WAV, FLAC, Vorbis); MP3 may be off by the codec delay.
    pub fn seek(&mut self, frame: u64) -> Result<()> {
        self.pending.clear();
        self.pending_pos = 0;
        self.skip = 0;
        self.eof = false;
        let to = match self.time_base {
            Some(tb) => {
                // ts = frame / rate / (numer / denom)
                let ts = u128::from(frame) * u128::from(tb.denom.get())
                    / (u128::from(tb.numer.get()) * u128::from(self.info.sample_rate));
                SeekTo::Timestamp {
                    ts: Timestamp::new(ts as i64),
                    track_id: self.track_id,
                }
            }
            None => SeekTo::Time {
                time: Time::try_from_secs_f64(frame as f64 / f64::from(self.info.sample_rate))
                    .unwrap_or_default(),
                track_id: Some(self.track_id),
            },
        };
        match self.format.seek(SeekMode::Accurate, to) {
            Ok(seeked) => {
                let delta = seeked.required_ts.get() - seeked.actual_ts.get();
                self.skip = self.ts_to_frames(delta.max(0) as u64);
            }
            Err(SymError::SeekError(_)) => self.eof = true,
            Err(e) => return Err(e.into()),
        }
        self.decoder.reset();
        self.position = frame;
        Ok(())
    }

    fn ts_to_frames(&self, ts: u64) -> u64 {
        match self.time_base {
            Some(tb) => {
                (u128::from(ts) * u128::from(tb.numer.get()) * u128::from(self.info.sample_rate)
                    / u128::from(tb.denom.get())) as u64
            }
            None => ts,
        }
    }

    /// Decode the next packet into `pending`. Returns false at end of stream.
    fn decode_next(&mut self) -> Result<bool> {
        loop {
            let packet = match self.format.next_packet() {
                Ok(Some(p)) => p,
                Ok(None) => {
                    self.eof = true;
                    return Ok(false);
                }
                Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                    self.eof = true;
                    return Ok(false);
                }
                Err(SymError::ResetRequired) => {
                    self.decoder.reset();
                    continue;
                }
                Err(e) => return Err(e.into()),
            };
            if packet.track_id != self.track_id {
                continue;
            }
            let buf = match self.decoder.decode(&packet) {
                Ok(buf) => buf,
                // A corrupt packet: skip it, like every player does.
                Err(SymError::DecodeError(_)) => continue,
                Err(e) => return Err(e.into()),
            };
            let src_ch = buf.spec().channels().count();
            let ch = self.info.channels;
            self.pending_pos = 0;
            if src_ch == ch || ch == 0 {
                buf.copy_to_vec_interleaved(&mut self.pending);
            } else {
                buf.copy_to_vec_interleaved(&mut self.scratch);
                let frames = self.scratch.len() / src_ch.max(1);
                self.pending.clear();
                self.pending.resize(frames * ch, 0.0);
                for f in 0..frames {
                    for c in 0..ch.min(src_ch) {
                        self.pending[f * ch + c] = self.scratch[f * src_ch + c];
                    }
                }
            }
            if self.skip > 0 && ch > 0 {
                let frames = (self.pending.len() / ch) as u64;
                let drop = self.skip.min(frames);
                self.pending_pos = drop as usize * ch;
                self.skip -= drop;
            }
            if self.pending_pos < self.pending.len() || ch == 0 {
                return Ok(true);
            }
        }
    }
}

/// Decode a whole file into memory at its own sample rate. For files this
/// program wrote or the caller otherwise trusts: memory use follows the
/// file's real length. See [`decode_file_within`].
pub fn decode_file(path: impl AsRef<Path>) -> Result<AudioData> {
    let data = decode_all(Decoder::open(path)?, u64::MAX)?;
    Ok(data.expect("a file cannot hold more samples than that"))
}

/// Decode a whole file into memory unless it holds more than `max_samples`
/// samples over all its channels, in which case nothing is kept and `None`
/// is returned. Samples, not seconds, since those are what memory follows:
/// the file picks its own rate and channel count. They are counted while
/// decoding, since the length in the header need not be true.
pub fn decode_file_within(path: impl AsRef<Path>, max_samples: u64) -> Result<Option<AudioData>> {
    decode_all(Decoder::open(path)?, max_samples)
}

fn decode_all(mut dec: Decoder, max_samples: u64) -> Result<Option<AudioData>> {
    let ch = dec.info().channels;
    let rate = dec.info().sample_rate;
    let max_frames = max_samples / ch.max(1) as u64;
    // The header's frame count is untrusted (truncated or corrupt files), so only use
    // it as a capped hint; the planes grow to the real decoded length.
    // Cap the total reservation (all channels), not per plane.
    const MAX_HINT_SAMPLES: u64 = 1 << 24;
    let hint = dec
        .info()
        .frames
        .unwrap_or(0)
        .min(max_frames)
        .min(MAX_HINT_SAMPLES / ch.max(1) as u64) as usize;
    let mut data = AudioData {
        sample_rate: rate,
        channels: (0..ch).map(|_| Vec::with_capacity(hint)).collect(),
    };
    let mut chunk = vec![0.0f32; 8192 * ch];
    loop {
        let n = dec.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        if data.frames() as u64 + n as u64 > max_frames {
            return Ok(None);
        }
        // Deinterleave straight into the planes; no second full-size copy.
        data.extend_interleaved(&chunk[..n * ch]);
    }
    Ok(Some(data))
}

/// [`decode_file_within`], then resampled to `sample_rate` if needed. The
/// limit is on the file as decoded, before resampling.
pub fn load_file_within(
    path: impl AsRef<Path>,
    sample_rate: u32,
    max_samples: u64,
) -> Result<Option<AudioData>> {
    decode_file_within(path, max_samples)?
        .map(|data| to_rate(data, sample_rate))
        .transpose()
}

fn to_rate(data: AudioData, sample_rate: u32) -> Result<AudioData> {
    if data.sample_rate == sample_rate {
        Ok(data)
    } else {
        resample(&data, sample_rate)
    }
}
