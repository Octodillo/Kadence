//! Incremental recording: the RT side pushes into a ring buffer, a writer thread drains
//! it into a WAV file and builds the waveform peaks as it goes.

use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use rtrb::{Consumer, Producer, RingBuffer};

use crate::peaks::PeakBuilder;
use crate::{MediaError, PeakCache, Result, WavFormat};

/// How often the writer thread drains the ring and rewrites the WAV header, so a crash
/// loses at most about this much audio.
const DRAIN_INTERVAL: Duration = Duration::from_millis(10);
const HEADER_EVERY: Duration = Duration::from_secs(1);

struct Shared {
    stop: AtomicBool,
    dropped_frames: AtomicU64,
}

/// What a finished recording produced.
#[derive(Clone, Debug)]
pub struct RecordingStats {
    pub path: PathBuf,
    pub frames: u64,
    /// Frames the RT side could not queue because the writer fell behind.
    pub dropped_frames: u64,
    pub peaks: PeakCache,
    /// Why `<file>.peaks` could not be written, if it wasn't. The cache is then rebuilt
    /// on the next load; the recording itself is fine.
    pub peaks_error: Option<String>,
}

/// The RT side of a recording. [`write`](Self::write) never blocks or allocates.
pub struct RecordInput {
    tx: Producer<f32>,
    shared: Arc<Shared>,
    channels: usize,
}

impl RecordInput {
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Queue one block: one slice per channel, equal lengths. If the ring is too full
    /// the whole block is dropped and counted. Returns the frames queued.
    pub fn write(&mut self, inputs: &[&[f32]]) -> usize {
        let ch = self.channels;
        let frames = inputs.iter().map(|c| c.len()).min().unwrap_or(0);
        if inputs.len() < ch {
            return 0;
        }
        let Ok(chunk) = self.tx.write_chunk_uninit(frames * ch) else {
            self.shared
                .dropped_frames
                .fetch_add(frames as u64, Ordering::Relaxed);
            return 0;
        };
        chunk.fill_from_iter((0..frames).flat_map(|f| (0..ch).map(move |c| inputs[c][f])));
        frames
    }
}

/// Owns the writer thread of one recording.
pub struct Recorder {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<Result<RecordingStats>>>,
}

impl Recorder {
    /// Create `path` and start the writer thread. The ring holds `buffer_seconds` of
    /// audio, which is how long the disk may stall before frames are dropped.
    pub fn start(
        path: impl AsRef<Path>,
        channels: usize,
        sample_rate: u32,
        format: WavFormat,
        buffer_seconds: f32,
    ) -> Result<(Recorder, RecordInput)> {
        let path = path.as_ref().to_owned();
        if channels == 0 {
            return Err(MediaError::Encode(
                "recording needs at least one channel".into(),
            ));
        }
        let file = File::create(&path).map_err(|e| MediaError::io(&path, e))?;
        let writer =
            hound::WavWriter::new(BufWriter::new(file), format.spec(channels, sample_rate))?;
        let capacity = ((buffer_seconds * sample_rate as f32) as usize).max(4096) * channels;
        let (tx, rx) = RingBuffer::new(capacity);
        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            dropped_frames: AtomicU64::new(0),
        });
        let thread = {
            let shared = shared.clone();
            std::thread::Builder::new()
                .name("daw-record".into())
                .spawn(move || writer_loop(rx, writer, format, channels, sample_rate, path, shared))
                .map_err(|e| MediaError::io("record thread", e))?
        };
        Ok((
            Recorder {
                shared: shared.clone(),
                thread: Some(thread),
            },
            RecordInput {
                tx,
                shared,
                channels,
            },
        ))
    }

    /// Frames dropped so far because the writer fell behind.
    pub fn dropped_frames(&self) -> u64 {
        self.shared.dropped_frames.load(Ordering::Relaxed)
    }

    /// Stop after writing everything already queued, finalize the file and save its
    /// peak cache. Drop the [`RecordInput`] (or stop calling it) first.
    pub fn finish(mut self) -> Result<RecordingStats> {
        self.shared.stop.store(true, Ordering::Release);
        self.thread
            .take()
            .expect("recorder thread")
            .join()
            .map_err(|_| MediaError::Encode("recording thread panicked".into()))?
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        if let Some(t) = self.thread.take() {
            self.shared.stop.store(true, Ordering::Release);
            let _ = t.join();
        }
    }
}

fn writer_loop(
    mut rx: Consumer<f32>,
    mut writer: hound::WavWriter<BufWriter<File>>,
    format: WavFormat,
    channels: usize,
    sample_rate: u32,
    path: PathBuf,
    shared: Arc<Shared>,
) -> Result<RecordingStats> {
    let mut peaks = PeakBuilder::new(channels, sample_rate);
    let mut since_header = Duration::ZERO;
    let mut frames = 0u64;
    let mut scratch = Vec::new();
    loop {
        // Read the flag before draining so samples queued before stop are never lost.
        let stopping = shared.stop.load(Ordering::Acquire) || rx.is_abandoned();
        let n = rx.slots() / channels * channels;
        if n > 0 {
            let chunk = rx.read_chunk(n).expect("slots checked");
            let (a, b) = chunk.as_slices();
            // The wrap point may split a frame, so join the halves before summarising.
            scratch.clear();
            scratch.extend_from_slice(a);
            scratch.extend_from_slice(b);
            chunk.commit_all();
            for &s in &scratch {
                format.write_sample(&mut writer, s)?;
            }
            peaks.push_interleaved(&scratch);
            frames += (n / channels) as u64;
        }
        if stopping {
            break;
        }
        std::thread::sleep(DRAIN_INTERVAL);
        since_header += DRAIN_INTERVAL;
        if since_header >= HEADER_EVERY {
            // Keeps the header's length fields current so a crash leaves a playable file.
            writer.flush()?;
            since_header = Duration::ZERO;
        }
    }
    writer.finalize()?;
    let peaks = peaks.finish();
    // The WAV is complete; a failed peak save must not turn the take into an error.
    let peaks_error = peaks.save_for(&path).err().map(|e| e.to_string());
    Ok(RecordingStats {
        peaks_error,
        path,
        frames,
        dropped_frames: shared.dropped_frames.load(Ordering::Relaxed),
        peaks,
    })
}
