//! Disk streaming for playback.
//!
//! A [`DiskStreamer`] owns one fill thread. Each opened file gets a pair of lock-free
//! ring buffers: interleaved samples, plus a small ring of segment headers that say
//! which file position (and which seek generation) the samples belong to. The fill
//! thread decodes, resamples to the engine rate and pushes; the RT side holds a
//! [`ClipReader`] and pulls from inside `Node::process` without blocking.
//!
//! Seeking is driven by the reader: asking for a position other than the one that
//! follows the previous read bumps a generation counter. The fill thread notices,
//! seeks the decoder and refills; the reader drops stale samples and plays silence
//! (reporting [`ReadStatus::Pending`]) until fresh data arrives.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::Duration;

use daw_core::time::SampleTime;
use rtrb::{Consumer, Producer, RingBuffer};

use crate::{AudioFileInfo, ChunkResampler, Decoder, Result};

/// Settings for a [`DiskStreamer`].
#[derive(Clone, Debug)]
pub struct StreamerConfig {
    /// Engine sample rate; every stream is resampled to it.
    pub sample_rate: u32,
    /// How much audio to keep buffered ahead of each reader.
    pub buffer_seconds: f32,
    /// Frames decoded per fill step.
    pub block_frames: usize,
    /// Sleep between fill passes when every buffer is full.
    pub poll_interval: Duration,
}

impl Default for StreamerConfig {
    fn default() -> Self {
        Self {
            sample_rate: 48_000,
            buffer_seconds: 2.0,
            block_frames: 4096,
            poll_interval: Duration::from_millis(2),
        }
    }
}

/// What a [`ClipReader::read`] call delivered.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ReadStatus {
    /// Every requested frame was real audio or legitimate silence (before the start or
    /// past the end of the file).
    Ok,
    /// Some frames were silent because a seek is still being serviced.
    Pending,
    /// Some frames were silent because the fill thread fell behind.
    Underrun,
}

#[derive(Copy, Clone, Debug)]
struct Segment {
    generation: u64,
    start: i64,
    frames: u32,
    eof: bool,
}

struct Shared {
    seek_generation: AtomicU64,
    seek_frame: AtomicI64,
    failed: AtomicBool,
}

/// Owns the fill thread. Dropping it stops the thread; readers then underrun.
pub struct DiskStreamer {
    config: StreamerConfig,
    tx: Option<mpsc::Sender<WorkerStream>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl DiskStreamer {
    pub fn new(config: StreamerConfig) -> Self {
        let (tx, rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let stop = stop.clone();
            let config = config.clone();
            std::thread::Builder::new()
                .name("daw-disk-stream".into())
                .spawn(move || worker(rx, stop, config))
                .expect("spawn disk streamer thread")
        };
        Self {
            config,
            tx: Some(tx),
            stop,
            thread: Some(thread),
        }
    }

    pub fn config(&self) -> &StreamerConfig {
        &self.config
    }

    /// Open `path` for streaming. Call from a normal thread; hand the reader to the RT
    /// side. Filling starts at frame 0 straight away.
    pub fn open(&self, path: impl AsRef<std::path::Path>) -> Result<ClipReader> {
        let decoder = Decoder::open(path)?;
        let info = Arc::new(decoder.info().clone());
        let ch = info.channels;
        let rate = self.config.sample_rate;
        let resampler = if info.sample_rate != rate {
            Some(ChunkResampler::new(info.sample_rate, rate, ch)?)
        } else {
            None
        };
        let buffer_frames =
            ((self.config.buffer_seconds * rate as f32) as usize).max(self.config.block_frames * 2);
        let (data_tx, data_rx) = RingBuffer::new(buffer_frames * ch);
        let (seg_tx, seg_rx) =
            RingBuffer::new(buffer_frames / self.config.block_frames.max(1) * 2 + 16);
        let shared = Arc::new(Shared {
            seek_generation: AtomicU64::new(0),
            seek_frame: AtomicI64::new(0),
            failed: AtomicBool::new(false),
        });
        let frames = info.frames.map(|f| {
            (u128::from(f) * u128::from(rate) / u128::from(info.sample_rate.max(1))) as u64
        });
        let stream = WorkerStream {
            _info: info.clone(),
            data: data_tx,
            segs: seg_tx,
            shared: shared.clone(),
            decoder,
            resampler,
            channels: ch,
            src_rate: info.sample_rate,
            dst_rate: rate,
            generation: 0,
            next_frame: 0,
            skip_to: 0,
            pending: Vec::new(),
            pending_pos: 0,
            scratch: vec![0.0; self.config.block_frames * ch],
            src_eof: false,
            eof_sent: false,
        };
        self.tx
            .as_ref()
            .expect("streamer running")
            .send(stream)
            .map_err(|_| crate::MediaError::Decode("disk streamer thread is gone".into()))?;
        Ok(ClipReader {
            data: data_rx,
            segs: seg_rx,
            shared,
            channels: ch,
            info,
            frames,
            generation: 0,
            current: None,
            expected: 0,
            eof_at: None,
            waiting: true,
            underruns: 0,
        })
    }
}

impl Drop for DiskStreamer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.tx = None;
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// The RT side of one streamed file. [`read`](Self::read) never allocates, locks or
/// does I/O. Dropping it on the RT thread only decrements reference counts while the
/// [`DiskStreamer`] is alive, because the fill thread holds the last references and
/// frees the buffers and file info. After the streamer is gone, drop readers off the RT
/// thread.
pub struct ClipReader {
    data: Consumer<f32>,
    segs: Consumer<Segment>,
    shared: Arc<Shared>,
    channels: usize,
    info: Arc<AudioFileInfo>,
    frames: Option<u64>,
    generation: u64,
    current: Option<Segment>,
    expected: i64,
    eof_at: Option<i64>,
    waiting: bool,
    underruns: u64,
}

impl ClipReader {
    /// The source file as decoded (its own rate, not the engine's).
    pub fn info(&self) -> &AudioFileInfo {
        &self.info
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Length at the engine rate, if the container states it.
    pub fn frames(&self) -> Option<u64> {
        self.frames
    }

    /// Reads that came up short because the fill thread was behind (not counting seeks).
    pub fn underruns(&self) -> u64 {
        self.underruns
    }

    /// True if the fill thread hit a decode error; the reader then behaves as at EOF.
    pub fn failed(&self) -> bool {
        self.shared.failed.load(Ordering::Relaxed)
    }

    /// Frames buffered and ready (may include stale frames from before a seek).
    pub fn buffered_frames(&self) -> usize {
        self.data.slots() / self.channels.max(1)
    }

    /// True once data for the current position (or the end of file) has arrived.
    pub fn is_ready(&mut self) -> bool {
        self.drop_stale();
        !self.waiting || self.eof_at.is_some()
    }

    /// Ask the fill thread to start buffering from `position` ahead of time, e.g. on a
    /// locate before playback resumes. RT-safe.
    pub fn cue(&mut self, position: SampleTime) {
        // Same position as already requested (or continuing): nothing to do, even while a
        // seek is still pending. Re-cueing would restart the seek every call.
        if position.0 == self.expected {
            return;
        }
        self.generation += 1;
        self.shared.seek_frame.store(position.0, Ordering::Relaxed);
        self.shared
            .seek_generation
            .store(self.generation, Ordering::Release);
        self.expected = position.0;
        // The rest of a half-read segment is still in the ring; drop it or every later
        // segment would be misaligned.
        if let Some(seg) = self.current.take() {
            self.discard(seg.frames as usize);
        }
        self.eof_at = None;
        self.waiting = true;
    }

    /// Fill `out` (one slice per output channel, equal lengths) with file audio starting
    /// at file frame `position` (engine rate). Positions before 0 or past the end are
    /// silent. A mono file is copied to every output; extra outputs are silent.
    pub fn read(&mut self, position: SampleTime, out: &mut [&mut [f32]]) -> ReadStatus {
        let n = out.iter().map(|c| c.len()).min().unwrap_or(0);
        if position.0 != self.expected {
            self.cue(position);
        }
        let mut status = ReadStatus::Ok;
        let mut i = 0usize;
        while i < n {
            let p = position.0 + i as i64;
            if p < 0 {
                let k = ((-p) as usize).min(n - i);
                silence(out, i, k);
                i += k;
                continue;
            }
            if self.eof_at.is_some_and(|e| p >= e) {
                silence(out, i, n - i);
                break;
            }
            let Some(mut seg) = self.next_segment() else {
                if let Some(end) = self.eof_at {
                    // Just reached the end marker. Anything before it with no data is a
                    // hole (should not happen); the check above handles the rest.
                    if p < end {
                        let k = ((end - p) as usize).min(n - i);
                        silence(out, i, k);
                        i += k;
                        status = ReadStatus::Underrun;
                    }
                    continue;
                }
                silence(out, i, n - i);
                if self.waiting {
                    status = ReadStatus::Pending;
                } else {
                    status = ReadStatus::Underrun;
                    self.underruns += 1;
                }
                break;
            };
            let end = seg.start + i64::from(seg.frames);
            if end <= p {
                self.discard(seg.frames as usize);
                self.current = None;
                continue;
            }
            if seg.start < p {
                let k = (p - seg.start) as usize;
                self.discard(k);
                seg.start += k as i64;
                seg.frames -= k as u32;
                self.current = Some(seg);
                continue;
            }
            if seg.start > p {
                // A hole should not happen; play silence over it rather than misalign.
                let k = ((seg.start - p) as usize).min(n - i);
                silence(out, i, k);
                i += k;
                status = ReadStatus::Underrun;
                continue;
            }
            let k = (seg.frames as usize).min(n - i);
            self.copy_out(out, i, k);
            seg.start += k as i64;
            seg.frames -= k as u32;
            self.current = (seg.frames > 0).then_some(seg);
            i += k;
        }
        self.expected = position.0 + n as i64;
        status
    }

    /// The segment being consumed, popping (and dropping stale) headers as needed.
    fn next_segment(&mut self) -> Option<Segment> {
        if let Some(seg) = self.current {
            return Some(seg);
        }
        while let Ok(seg) = self.segs.pop() {
            // While a seek is pending, data that ends before the position the reader has
            // moved on to doesn't serve it; dropping it keeps the read Pending rather
            // than reporting an underrun (the transport may roll during a locate).
            let behind = self.waiting && seg.start + i64::from(seg.frames) <= self.expected;
            if seg.generation != self.generation || (behind && !seg.eof) {
                self.discard(seg.frames as usize);
                continue;
            }
            self.waiting = false;
            if seg.eof {
                self.eof_at = Some(seg.start);
                return None;
            }
            self.current = Some(seg);
            return self.current;
        }
        None
    }

    /// Throw away everything already known to be from an old generation.
    fn drop_stale(&mut self) {
        if self.current.is_none() {
            let _ = self.next_segment();
        }
    }

    fn discard(&mut self, frames: usize) {
        let n = (frames * self.channels).min(self.data.slots());
        if let Ok(chunk) = self.data.read_chunk(n) {
            chunk.commit_all();
        }
    }

    fn copy_out(&mut self, out: &mut [&mut [f32]], at: usize, frames: usize) {
        let ch = self.channels;
        let Ok(chunk) = self.data.read_chunk(frames * ch) else {
            silence(out, at, frames);
            return;
        };
        let (a, b) = chunk.as_slices();
        let sample = |idx: usize| {
            if idx < a.len() {
                a[idx]
            } else {
                b[idx - a.len()]
            }
        };
        let mono = ch == 1;
        for (oc, dst) in out.iter_mut().enumerate() {
            let dst = &mut dst[at..at + frames];
            if mono {
                for (f, d) in dst.iter_mut().enumerate() {
                    *d = sample(f);
                }
            } else if oc < ch {
                for (f, d) in dst.iter_mut().enumerate() {
                    *d = sample(f * ch + oc);
                }
            } else {
                dst.fill(0.0);
            }
        }
        chunk.commit_all();
    }
}

fn silence(out: &mut [&mut [f32]], at: usize, frames: usize) {
    for c in out.iter_mut() {
        c[at..at + frames].fill(0.0);
    }
}

/// The fill-thread side of one stream.
struct WorkerStream {
    /// Keeps the reader's file info alive so its last drop happens on this thread.
    _info: Arc<AudioFileInfo>,
    data: Producer<f32>,
    segs: Producer<Segment>,
    shared: Arc<Shared>,
    decoder: Decoder,
    resampler: Option<ChunkResampler>,
    channels: usize,
    src_rate: u32,
    dst_rate: u32,
    generation: u64,
    next_frame: i64,
    /// First frame to push after a seek; earlier decoded frames are dropped.
    skip_to: i64,
    /// Decoded (and resampled) interleaved frames not yet pushed.
    pending: Vec<f32>,
    pending_pos: usize,
    scratch: Vec<f32>,
    src_eof: bool,
    eof_sent: bool,
}

impl WorkerStream {
    /// Push at most one block. Returns true if it did anything.
    fn fill_once(&mut self, block_frames: usize) -> bool {
        let g = self.shared.seek_generation.load(Ordering::Acquire);
        if g != self.generation {
            self.generation = g;
            let target = self.shared.seek_frame.load(Ordering::Relaxed);
            self.seek(target);
        }
        if self.eof_sent || self.segs.slots() == 0 {
            return false;
        }
        let ch = self.channels;
        let room = self.data.slots() / ch;
        if room < block_frames.min(4096) {
            return false;
        }
        let want = block_frames.min(room);
        loop {
            let avail = (self.pending.len() - self.pending_pos) / ch;
            // A grid-aligned seek starts a little before the target: drop those frames
            // here so every pushed segment starts at or after the requested position.
            if self.next_frame < self.skip_to && avail > 0 {
                let k = ((self.skip_to - self.next_frame) as usize).min(avail);
                self.pending_pos += k * ch;
                self.next_frame += k as i64;
                continue;
            }
            if avail >= want || self.src_eof {
                break;
            }
            if let Err(()) = self.decode_more() {
                self.shared.failed.store(true, Ordering::Relaxed);
                self.src_eof = true;
            }
        }
        let avail = (self.pending.len() - self.pending_pos) / ch;
        let k = avail.min(want);
        if k > 0 {
            let src = &self.pending[self.pending_pos..self.pending_pos + k * ch];
            if let Ok(chunk) = self.data.write_chunk_uninit(k * ch) {
                chunk.fill_from_iter(src.iter().copied());
            }
            self.pending_pos += k * ch;
            if self.pending_pos * 2 > self.pending.len() {
                self.pending.drain(..self.pending_pos);
                self.pending_pos = 0;
            }
            let _ = self.segs.push(Segment {
                generation: self.generation,
                start: self.next_frame,
                frames: k as u32,
                eof: false,
            });
            self.next_frame += k as i64;
        }
        // Announce the end straight after the last data so readers never see a gap there.
        let drained = self.pending_pos >= self.pending.len();
        if self.src_eof && drained && self.segs.slots() > 0 {
            let _ = self.segs.push(Segment {
                generation: self.generation,
                start: self.next_frame,
                frames: 0,
                eof: true,
            });
            self.eof_sent = true;
        }
        true
    }

    fn decode_more(&mut self) -> std::result::Result<(), ()> {
        let n = self.decoder.read(&mut self.scratch).map_err(|_| ())?;
        let input = &self.scratch[..n * self.channels];
        match &mut self.resampler {
            Some(rs) if n == 0 => rs.flush(&mut self.pending).map_err(|_| ())?,
            Some(rs) => rs.process(input, &mut self.pending).map_err(|_| ())?,
            None => self.pending.extend_from_slice(input),
        }
        if n == 0 {
            self.src_eof = true;
        }
        Ok(())
    }

    fn seek(&mut self, target: i64) {
        let requested = target.max(0);
        // A freshly reset resampler rings for its filter length, so start decoding a
        // little earlier and let the skip below drop the lead-in.
        let lead = self
            .resampler
            .as_ref()
            .map_or(0, |rs| rs.warmup_frames() as i64);
        let target = u128::from((requested - lead).max(0) as u64);
        let (src_rate, dst_rate) = (u128::from(self.src_rate), u128::from(self.dst_rate));
        // Start on a frame where source and engine grids coincide, so the resampled
        // output lands exactly on engine frames; the reader drops the frames before
        // `target`. Without resampling both periods are 1. For odd rate pairs the period
        // gets long (e.g. 44101 -> 48000 is a whole second), so past a small bound fall
        // back to the nearest source frame and accept a sub-frame offset.
        const MAX_ALIGN: u128 = 1024;
        let g = u128::from(gcd(self.src_rate, self.dst_rate).max(1));
        let (src_period, dst_period) = ((src_rate / g).max(1), (dst_rate / g).max(1));
        let (src, start) = if dst_period <= MAX_ALIGN {
            let periods = target / dst_period;
            (periods * src_period, periods * dst_period)
        } else {
            // Nearest source frame (rounded, not floored).
            ((target * src_rate + dst_rate / 2) / dst_rate.max(1), target)
        };
        let src = u64::try_from(src).unwrap_or(u64::MAX);
        let start = i64::try_from(start).unwrap_or(i64::MAX);
        self.pending.clear();
        self.pending_pos = 0;
        self.eof_sent = false;
        self.src_eof = false;
        if let Some(rs) = &mut self.resampler {
            rs.reset();
        }
        if self.decoder.seek(src).is_err() {
            self.src_eof = true;
        }
        self.next_frame = start;
        self.skip_to = requested;
    }
}

fn gcd(mut a: u32, mut b: u32) -> u32 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

fn worker(rx: mpsc::Receiver<WorkerStream>, stop: Arc<AtomicBool>, config: StreamerConfig) {
    let mut streams: Vec<WorkerStream> = Vec::new();
    while !stop.load(Ordering::Relaxed) {
        loop {
            match rx.try_recv() {
                Ok(s) => streams.push(s),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    if streams.is_empty() {
                        return;
                    }
                    break;
                }
            }
        }
        // Round-robin one block per stream per pass so every stream gets fed fairly.
        loop {
            let mut busy = false;
            for s in &mut streams {
                busy |= s.fill_once(config.block_frames);
            }
            if !busy || stop.load(Ordering::Relaxed) {
                break;
            }
        }
        // The reader went away: drop our end, which frees the buffers here, not on the RT thread.
        streams.retain(|s| !s.data.is_abandoned());
        std::thread::sleep(config.poll_interval);
    }
}
