//! Multi-resolution min/max waveform summaries, cached on disk next to the audio file
//! as `<file>.peaks`.

use std::fs;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use crate::{Decoder, MediaError, Result};

const MAGIC: &[u8; 8] = b"DAWPEAKS";
const VERSION: u32 = 1;
/// Frames per peak at the finest level. Closer zoom reads samples directly.
pub const BASE_FRAMES_PER_PEAK: u32 = 64;
/// Each level summarises this many peaks of the level below.
const LEVEL_FACTOR: u32 = 4;
const NUM_LEVELS: usize = 6; // 64 .. 65536 frames per peak

/// One resolution of the summary. `data` holds `[min, max]` pairs as `i16`, interleaved
/// by peak then channel: `data[(peak * channels + ch) * 2 ..][..2]`.
#[derive(Clone, Debug, PartialEq)]
pub struct PeakLevel {
    pub frames_per_peak: u32,
    pub data: Vec<i16>,
}

impl PeakLevel {
    pub fn len(&self, channels: usize) -> usize {
        self.data.len() / (2 * channels.max(1))
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }
}

/// The waveform summary for one audio file.
#[derive(Clone, Debug, PartialEq)]
pub struct PeakCache {
    pub sample_rate: u32,
    pub channels: usize,
    pub frames: u64,
    pub levels: Vec<PeakLevel>,
}

fn to_i16(s: f32) -> i16 {
    (s * 32767.0).round().clamp(-32768.0, 32767.0) as i16
}

fn from_i16(s: i16) -> f32 {
    f32::from(s) / 32767.0
}

/// Accumulates peaks from interleaved audio pushed in any chunk size. Recording uses
/// this to build the cache while it writes.
pub struct PeakBuilder {
    channels: usize,
    sample_rate: u32,
    frames: u64,
    in_peak: u32,
    acc: Vec<[f32; 2]>,
    base: Vec<i16>,
}

impl PeakBuilder {
    pub fn new(channels: usize, sample_rate: u32) -> Self {
        Self {
            channels,
            sample_rate,
            frames: 0,
            in_peak: 0,
            acc: vec![[f32::INFINITY, f32::NEG_INFINITY]; channels],
            base: Vec::new(),
        }
    }

    pub fn push_interleaved(&mut self, samples: &[f32]) {
        let ch = self.channels;
        let frames = samples.len() / ch;
        let mut at = 0;
        // Work a whole peak's run per channel at a time; much faster than frame by frame.
        while at < frames {
            let take = ((BASE_FRAMES_PER_PEAK - self.in_peak) as usize).min(frames - at);
            let run = &samples[at * ch..(at + take) * ch];
            for (c, a) in self.acc.iter_mut().enumerate() {
                let (mut lo, mut hi) = (a[0], a[1]);
                for s in run.iter().skip(c).step_by(ch) {
                    if *s < lo {
                        lo = *s;
                    }
                    if *s > hi {
                        hi = *s;
                    }
                }
                *a = [lo, hi];
            }
            self.in_peak += take as u32;
            at += take;
            if self.in_peak == BASE_FRAMES_PER_PEAK {
                self.emit();
            }
        }
        self.frames += frames as u64;
    }

    fn emit(&mut self) {
        for a in &mut self.acc {
            self.base.push(to_i16(a[0]));
            self.base.push(to_i16(a[1]));
            *a = [f32::INFINITY, f32::NEG_INFINITY];
        }
        self.in_peak = 0;
    }

    pub fn finish(mut self) -> PeakCache {
        if self.in_peak > 0 {
            self.emit();
        }
        let ch = self.channels;
        let mut levels = vec![PeakLevel {
            frames_per_peak: BASE_FRAMES_PER_PEAK,
            data: std::mem::take(&mut self.base),
        }];
        while levels.len() < NUM_LEVELS {
            let prev = levels.last().unwrap();
            let group = 2 * ch * LEVEL_FACTOR as usize;
            let mut data = Vec::with_capacity(prev.data.len() / LEVEL_FACTOR as usize + 2 * ch);
            for chunk in prev.data.chunks(group) {
                for c in 0..ch {
                    let (mut lo, mut hi) = (i16::MAX, i16::MIN);
                    for p in chunk.chunks_exact(2 * ch) {
                        lo = lo.min(p[2 * c]);
                        hi = hi.max(p[2 * c + 1]);
                    }
                    data.push(lo);
                    data.push(hi);
                }
            }
            let frames_per_peak = prev.frames_per_peak * LEVEL_FACTOR;
            levels.push(PeakLevel {
                frames_per_peak,
                data,
            });
        }
        PeakCache {
            sample_rate: self.sample_rate,
            channels: ch,
            frames: self.frames,
            levels,
        }
    }
}

/// Size and mtime of the source, used to tell whether a cache is stale.
fn source_stamp(audio: &Path) -> Result<(u64, i64)> {
    let meta = fs::metadata(audio).map_err(|e| MediaError::io(audio, e))?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos() as i64);
    Ok((meta.len(), mtime))
}

impl PeakCache {
    /// Where the cache for `audio` lives: the same path with `.peaks` appended.
    pub fn path_for(audio: impl AsRef<Path>) -> PathBuf {
        let mut p = audio.as_ref().as_os_str().to_owned();
        p.push(".peaks");
        PathBuf::from(p)
    }

    /// Decode `audio` and build its summary (does not touch the disk cache).
    pub fn build(audio: impl AsRef<Path>) -> Result<Self> {
        let mut dec = Decoder::open(audio)?;
        let ch = dec.info().channels;
        let mut builder = PeakBuilder::new(ch, dec.info().sample_rate);
        let mut buf = vec![0.0f32; 16384 * ch];
        loop {
            let n = dec.read(&mut buf)?;
            if n == 0 {
                break;
            }
            builder.push_interleaved(&buf[..n * ch]);
        }
        Ok(builder.finish())
    }

    /// Load the cache next to `audio` if it is current, otherwise build and save it.
    pub fn load_or_build(audio: impl AsRef<Path>) -> Result<Self> {
        let audio = audio.as_ref();
        let stamp = source_stamp(audio)?;
        let cache_path = Self::path_for(audio);
        if let Ok(cache) = Self::load(&cache_path, stamp) {
            return Ok(cache);
        }
        let cache = Self::build(audio)?;
        cache.save(&cache_path, stamp)?;
        Ok(cache)
    }

    /// Write the cache for `audio` to its `.peaks` path.
    pub fn save_for(&self, audio: impl AsRef<Path>) -> Result<()> {
        let audio = audio.as_ref();
        self.save(&Self::path_for(audio), source_stamp(audio)?)
    }

    fn save(&self, path: &Path, (len, mtime): (u64, i64)) -> Result<()> {
        let io = |e| MediaError::io(path, e);
        // Write to a temp name and rename so readers never see a half-written cache.
        let tmp = path.with_extension("peaks.tmp");
        let mut w = BufWriter::new(fs::File::create(&tmp).map_err(io)?);
        let mut put = |b: &[u8]| w.write_all(b).map_err(io);
        put(MAGIC)?;
        put(&VERSION.to_le_bytes())?;
        put(&len.to_le_bytes())?;
        put(&mtime.to_le_bytes())?;
        put(&self.sample_rate.to_le_bytes())?;
        put(&(self.channels as u32).to_le_bytes())?;
        put(&self.frames.to_le_bytes())?;
        put(&(self.levels.len() as u32).to_le_bytes())?;
        for level in &self.levels {
            put(&level.frames_per_peak.to_le_bytes())?;
            put(&(level.data.len() as u64).to_le_bytes())?;
            let bytes: Vec<u8> = level.data.iter().flat_map(|s| s.to_le_bytes()).collect();
            put(&bytes)?;
        }
        w.into_inner()
            .map_err(|e| io(e.into_error()))?
            .sync_all()
            .map_err(io)?;
        fs::rename(&tmp, path).map_err(io)
    }

    fn load(path: &Path, stamp: (u64, i64)) -> Result<Self> {
        let io = |e| MediaError::io(path, e);
        let bad = |what: &str| MediaError::Peaks(format!("{}: {what}", path.display()));
        let mut r = BufReader::new(fs::File::open(path).map_err(io)?);
        let mut magic = [0u8; 8];
        r.read_exact(&mut magic).map_err(io)?;
        if &magic != MAGIC {
            return Err(bad("not a peak file"));
        }
        macro_rules! get {
            ($t:ty) => {{
                let mut b = [0u8; std::mem::size_of::<$t>()];
                r.read_exact(&mut b).map_err(io)?;
                <$t>::from_le_bytes(b)
            }};
        }
        if get!(u32) != VERSION {
            return Err(bad("unknown version"));
        }
        if (get!(u64), get!(i64)) != stamp {
            return Err(bad("stale"));
        }
        let sample_rate = get!(u32);
        let channels = get!(u32) as usize;
        let frames = get!(u64);
        let n = get!(u32) as usize;
        if n > 64 || channels == 0 {
            return Err(bad("corrupt header"));
        }
        let mut levels = Vec::with_capacity(n);
        for _ in 0..n {
            let frames_per_peak = get!(u32);
            let len = get!(u64) as usize;
            let expected =
                frames.div_ceil(u64::from(frames_per_peak.max(1))) as usize * 2 * channels;
            if len != expected {
                return Err(bad("corrupt level"));
            }
            let mut bytes = vec![0u8; len * 2];
            r.read_exact(&mut bytes).map_err(io)?;
            let data = bytes
                .chunks_exact(2)
                .map(|b| i16::from_le_bytes([b[0], b[1]]))
                .collect();
            levels.push(PeakLevel {
                frames_per_peak,
                data,
            });
        }
        Ok(Self {
            sample_rate,
            channels,
            frames,
            levels,
        })
    }

    /// Min/max of `channel` for `width` equal columns spanning frames `start..end`.
    /// Uses the coarsest level that still has at least one peak per column. `out` is
    /// cleared and refilled with `(min, max)` pairs in -1..1.
    pub fn query(
        &self,
        channel: usize,
        start: u64,
        end: u64,
        width: usize,
        out: &mut Vec<(f32, f32)>,
    ) {
        out.clear();
        if width == 0 || end <= start || channel >= self.channels {
            return;
        }
        let per_col = (end - start) as f64 / width as f64;
        let level = self
            .levels
            .iter()
            .rev()
            .find(|l| f64::from(l.frames_per_peak) <= per_col)
            .unwrap_or(&self.levels[0]);
        let fpp = u64::from(level.frames_per_peak);
        let count = level.len(self.channels) as u64;
        let ch = self.channels;
        for col in 0..width {
            let f0 = start + (col as f64 * per_col) as u64;
            let f1 = (start + ((col + 1) as f64 * per_col) as u64).max(f0 + 1);
            let p0 = f0 / fpp;
            let p1 = f1.div_ceil(fpp).min(count);
            if p0 >= p1 {
                out.push((0.0, 0.0));
                continue;
            }
            let (mut lo, mut hi) = (i16::MAX, i16::MIN);
            for p in p0..p1 {
                let i = (p as usize * ch + channel) * 2;
                lo = lo.min(level.data[i]);
                hi = hi.max(level.data[i + 1]);
            }
            out.push((from_i16(lo), from_i16(hi)));
        }
    }
}
