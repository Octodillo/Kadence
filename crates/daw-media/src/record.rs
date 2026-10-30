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

/// Largest data chunk written to one file. RIFF stores sizes as `u32` and hound does
/// not check them, so a take is split into several files well before 4 GiB. The
/// margin covers the header and keeps the RIFF size clear of `u32::MAX`, which
/// readers take to mean "unknown length".
const MAX_DATA_BYTES: u64 = u32::MAX as u64 - (1 << 20);

struct Shared {
    stop: AtomicBool,
    /// Set by the writer when it hit an error and stopped writing.
    failed: AtomicBool,
    dropped_frames: AtomicU64,
}

/// One file of a recording.
#[derive(Clone, Debug)]
pub struct RecordedFile {
    pub path: PathBuf,
    pub frames: u64,
    pub peaks: PeakCache,
    /// Why `<file>.peaks` could not be written, if it wasn't. The cache is then rebuilt
    /// on the next load; the recording itself is fine.
    pub peaks_error: Option<Arc<MediaError>>,
}

/// What a finished recording produced.
#[derive(Clone, Debug)]
pub struct RecordingStats {
    /// The take, in order. A WAV file holds at most 4 GiB, so a long take continues
    /// gaplessly in `<stem>-2.<ext>`, `<stem>-3.<ext>` and so on. The first file is
    /// the path given to [`Recorder::start`].
    pub files: Vec<RecordedFile>,
    /// Frames written, over all files.
    pub frames: u64,
    /// Frames the RT side could not queue because the writer fell behind.
    pub dropped_frames: u64,
    /// The error that stopped the writer early (disk full, drive removed). The files
    /// hold the audio written before it; check this before treating the take as whole.
    /// The last file's `frames` then counts what was handed to the OS, which can be a
    /// few KiB more than reached the disk.
    pub error: Option<Arc<MediaError>>,
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

    /// True once the writer has stopped on an error. Nothing more is recorded; stop
    /// and call [`Recorder::finish`] to get the error and what was saved.
    pub fn failed(&self) -> bool {
        self.shared.failed.load(Ordering::Relaxed)
    }

    /// Queue one block: one slice per channel, equal lengths. If the ring is too full
    /// the frames that don't fit are dropped and counted, as is a whole block with
    /// missing channels. Returns the frames queued, which is 0 once the writer has
    /// [`failed`](Self::failed).
    pub fn write(&mut self, inputs: &[&[f32]]) -> usize {
        let ch = self.channels;
        let frames = inputs.iter().map(|c| c.len()).min().unwrap_or(0);
        if self.failed() {
            return 0;
        }
        let fit = if inputs.len() < ch {
            0
        } else {
            frames.min(self.tx.slots() / ch)
        };
        if fit < frames {
            self.shared
                .dropped_frames
                .fetch_add((frames - fit) as u64, Ordering::Relaxed);
        }
        if fit == 0 {
            return 0;
        }
        let chunk = self.tx.write_chunk_uninit(fit * ch).expect("slots checked");
        chunk.fill_from_iter((0..fit).flat_map(|f| (0..ch).map(move |c| inputs[c][f])));
        fit
    }
}

/// Owns the writer thread of one recording.
pub struct Recorder {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<RecordingStats>>,
}

impl Recorder {
    /// Create `path` and start the writer thread. The ring holds `buffer_seconds` of
    /// audio, which is how long the disk may stall before frames are dropped. Fails if
    /// `path` already exists: an earlier take there is never overwritten.
    pub fn start(
        path: impl AsRef<Path>,
        channels: usize,
        sample_rate: u32,
        format: WavFormat,
        buffer_seconds: f32,
    ) -> Result<(Recorder, RecordInput)> {
        let path = path.as_ref();
        Self::start_with_limit(
            path,
            File::create_new(path),
            channels,
            sample_rate,
            format,
            buffer_seconds,
            MAX_DATA_BYTES,
        )
    }

    fn start_with_limit(
        path: &Path,
        file: std::io::Result<File>,
        channels: usize,
        sample_rate: u32,
        format: WavFormat,
        buffer_seconds: f32,
        max_data_bytes: u64,
    ) -> Result<(Recorder, RecordInput)> {
        if channels == 0 || channels > u16::MAX as usize {
            return Err(MediaError::Encode(format!("bad channel count {channels}")));
        }
        let spec = format.spec(channels, sample_rate);
        let frame_bytes = channels as u64 * spec.bits_per_sample as u64 / 8;
        let max_frames = (max_data_bytes / frame_bytes).max(1);
        let first = Segment::create(path.to_owned(), file, spec, sample_rate)?;
        let capacity = ((buffer_seconds * sample_rate as f32) as usize).max(4096) * channels;
        let (tx, rx) = RingBuffer::new(capacity);
        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            failed: AtomicBool::new(false),
            dropped_frames: AtomicU64::new(0),
        });
        let thread = {
            let shared = shared.clone();
            let w = Writer {
                format,
                spec,
                sample_rate,
                channels,
                max_frames,
                base: path.to_owned(),
                current: Some(first),
                done: Vec::new(),
                frames: 0,
            };
            std::thread::Builder::new()
                .name("daw-record".into())
                .spawn(move || writer_loop(rx, w, shared))
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

    /// True once the writer has stopped on an error; see [`RecordInput::failed`].
    pub fn failed(&self) -> bool {
        self.shared.failed.load(Ordering::Relaxed)
    }

    /// Stop after writing everything already queued, finalize the files and save their
    /// peak caches. Drop the [`RecordInput`] (or stop calling it) first.
    ///
    /// A writer error does not make this fail: the stats list what was saved and carry
    /// the error in [`RecordingStats::error`]. `Err` means the writer thread panicked.
    pub fn finish(mut self) -> Result<RecordingStats> {
        self.shared.stop.store(true, Ordering::Release);
        self.thread
            .take()
            .expect("recorder thread")
            .join()
            .map_err(|_| MediaError::Encode("recording thread panicked".into()))
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

/// One open file of a take.
struct Segment {
    path: PathBuf,
    writer: hound::WavWriter<BufWriter<File>>,
    peaks: PeakBuilder,
    frames: u64,
    /// A write to this file failed, so its contents past the last header update are
    /// unknown.
    failed: bool,
}

impl Segment {
    fn create(
        path: PathBuf,
        file: std::io::Result<File>,
        spec: hound::WavSpec,
        sample_rate: u32,
    ) -> Result<Segment> {
        let file = file.map_err(|e| MediaError::io(&path, e))?;
        Ok(Segment {
            writer: hound::WavWriter::new(BufWriter::new(file), spec)?,
            peaks: PeakBuilder::new(spec.channels as usize, sample_rate),
            frames: 0,
            failed: false,
            path,
        })
    }

    /// Finalize the file and save its peaks. The [`RecordedFile`] is returned even if
    /// finalizing failed, since the file still holds the audio up to the last header
    /// update.
    fn close(self) -> (RecordedFile, Result<()>) {
        let finalized = self.writer.finalize().map_err(MediaError::from);
        let peaks = self.peaks.finish();
        // After a write error the cache may not match the file (and the disk is likely
        // full), so leave it to be rebuilt from the file on the next load.
        let peaks_error = if self.failed {
            Some(Arc::new(MediaError::Peaks(
                "not saved because the recording failed".into(),
            )))
        } else {
            peaks.save_for(&self.path).err().map(Arc::new)
        };
        let file = RecordedFile {
            path: self.path,
            frames: self.frames,
            peaks,
            peaks_error,
        };
        (file, finalized)
    }
}

struct Writer {
    format: WavFormat,
    spec: hound::WavSpec,
    sample_rate: u32,
    channels: usize,
    max_frames: u64,
    base: PathBuf,
    current: Option<Segment>,
    done: Vec<RecordedFile>,
    frames: u64,
}

impl Writer {
    /// Write interleaved whole frames, starting a new file whenever one is full.
    fn write(&mut self, mut samples: &[f32]) -> Result<()> {
        while !samples.is_empty() {
            if self
                .current
                .as_ref()
                .is_some_and(|s| s.frames >= self.max_frames)
            {
                self.next_segment()?;
            }
            let seg = self.current.as_mut().expect("open segment");
            let room = ((self.max_frames - seg.frames) as usize).saturating_mul(self.channels);
            let (now, rest) = samples.split_at(samples.len().min(room));
            let mut written = 0;
            let mut result = Ok(());
            'frames: for frame in now.chunks_exact(self.channels) {
                for &s in frame {
                    if let Err(e) = self.format.write_sample(&mut seg.writer, s) {
                        result = Err(e);
                        break 'frames;
                    }
                }
                written += frame.len();
            }
            let frames = (written / self.channels) as u64;
            seg.frames += frames;
            self.frames += frames;
            seg.peaks.push_interleaved(&now[..written]);
            if let Err(e) = result {
                seg.failed = true;
                return Err(e.into());
            }
            samples = rest;
        }
        Ok(())
    }

    fn next_segment(&mut self) -> Result<()> {
        let (file, finalized) = self.current.take().expect("open segment").close();
        self.done.push(file);
        finalized?;
        let n = self.done.len() + 1;
        let stem = self.base.file_stem().unwrap_or_default();
        let ext = self.base.extension();
        // Never overwrite an existing file; skip to the next free name.
        for i in n.. {
            let mut name = stem.to_owned();
            name.push(format!("-{i}"));
            if let Some(ext) = ext {
                name.push(".");
                name.push(ext);
            }
            let path = self.base.with_file_name(name);
            match File::create_new(&path) {
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                file => {
                    self.current = Some(Segment::create(path, file, self.spec, self.sample_rate)?);
                    return Ok(());
                }
            }
        }
        unreachable!("ran out of file names")
    }

    fn flush(&mut self) -> Result<()> {
        if let Some(seg) = &mut self.current
            && let Err(e) = seg.writer.flush()
        {
            seg.failed = true;
            return Err(e.into());
        }
        Ok(())
    }

    fn finish(mut self, error: Option<MediaError>, dropped_frames: u64) -> RecordingStats {
        let mut error = error;
        if let Some(seg) = self.current.take() {
            let (file, finalized) = seg.close();
            self.done.push(file);
            if let Err(e) = finalized {
                error.get_or_insert(e);
            }
        }
        RecordingStats {
            files: self.done,
            frames: self.frames,
            dropped_frames,
            error: error.map(Arc::new),
        }
    }
}

fn writer_loop(mut rx: Consumer<f32>, mut w: Writer, shared: Arc<Shared>) -> RecordingStats {
    let mut since_header = Duration::ZERO;
    let mut scratch = Vec::new();
    let error = loop {
        // Read the flag before draining so samples queued before stop are never lost.
        let stopping = shared.stop.load(Ordering::Acquire) || rx.is_abandoned();
        let n = rx.slots() / w.channels * w.channels;
        if n > 0 {
            let chunk = rx.read_chunk(n).expect("slots checked");
            let (a, b) = chunk.as_slices();
            // The wrap point may split a frame, so join the halves before writing.
            scratch.clear();
            scratch.extend_from_slice(a);
            scratch.extend_from_slice(b);
            chunk.commit_all();
            if let Err(e) = w.write(&scratch) {
                break Some(e);
            }
        }
        if stopping {
            break None;
        }
        std::thread::sleep(DRAIN_INTERVAL);
        since_header += DRAIN_INTERVAL;
        if since_header >= HEADER_EVERY {
            // Keeps the header's length fields current so a crash leaves a playable file.
            if let Err(e) = w.flush() {
                break Some(e);
            }
            since_header = Duration::ZERO;
        }
    };
    if error.is_some() {
        shared.failed.store(true, Ordering::Relaxed);
        // Keep the ring until the RT side lets go or we are told to stop, so it is
        // never freed on the RT thread.
        while !shared.stop.load(Ordering::Acquire) && !rx.is_abandoned() {
            std::thread::sleep(DRAIN_INTERVAL);
        }
    }
    let dropped = shared.dropped_frames.load(Ordering::Relaxed);
    w.finish(error, dropped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode_file;

    /// Issue #22: a writer I/O error (disk full) used to make `finish` return only the
    /// error, losing the stats, while the RT side kept "recording" into a dead ring.
    #[cfg(target_os = "linux")]
    #[test]
    fn writer_error_is_reported_at_once_and_keeps_the_stats() {
        let full = Path::new("/dev/full");
        let file = File::options().write(true).open(full);
        let (rec, mut input) = Recorder::start_with_limit(
            full,
            file,
            2,
            48_000,
            WavFormat::Float32,
            1.0,
            MAX_DATA_BYTES,
        )
        .unwrap();
        let block = vec![0.1f32; 512];
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        // BufWriter holds 8 KiB, so the error shows up on a later write or header flush.
        while !input.failed() {
            assert!(
                std::time::Instant::now() < deadline,
                "writer error never surfaced"
            );
            input.write(&[&block, &block]);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(rec.failed());
        let dropped_before = rec.dropped_frames();
        assert_eq!(
            input.write(&[&block, &block]),
            0,
            "nothing is queued after a failure"
        );
        assert_eq!(rec.dropped_frames(), dropped_before);
        drop(input);
        let stats = rec
            .finish()
            .expect("a writer error still returns the stats");
        let err = stats.error.expect("the error is reported");
        assert!(err.to_string().contains("space"), "{err}");
        assert_eq!(stats.files.len(), 1);
        // No peak cache is written for a failed file (here it would be /dev/full.peaks).
        assert!(stats.files[0].peaks_error.is_some());
        assert!(!Path::new("/dev/full.peaks").exists());
    }

    #[test]
    fn file_limit_leaves_room_for_the_header() {
        // hound's largest header (WAVE_FORMAT_EXTENSIBLE) is well under 1 KiB.
        assert!(MAX_DATA_BYTES + 1024 < u32::MAX as u64);
    }

    /// Issue #22: past 4 GiB hound's u32 byte count overflowed (panic in debug, a
    /// wrapped header in release). A full file now continues in a new one with no
    /// frame lost; the limit is shrunk here so the test runs in milliseconds.
    #[test]
    fn long_take_continues_in_new_files_without_overwriting() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("take.wav");
        let taken = dir.path().join("take-2.wav");
        std::fs::write(&taken, b"not ours").unwrap();
        // 1000 stereo Pcm16 frames (4000 bytes) per file.
        let (rec, mut input) = Recorder::start_with_limit(
            &path,
            File::create_new(&path),
            2,
            48_000,
            WavFormat::Pcm16,
            1.0,
            4000,
        )
        .unwrap();
        let left: Vec<f32> = (0..2500).map(|i| (i as f32 / 2500.0) - 0.5).collect();
        let right: Vec<f32> = left.iter().map(|s| -s).collect();
        for (l, r) in left.chunks(300).zip(right.chunks(300)) {
            assert_eq!(input.write(&[l, r]), l.len());
        }
        drop(input);
        let stats = rec.finish().unwrap();
        assert!(stats.error.is_none(), "{:?}", stats.error);
        assert_eq!(stats.frames, 2500);
        let names: Vec<_> = stats.files.iter().map(|f| f.path.clone()).collect();
        assert_eq!(
            names,
            [
                path.clone(),
                dir.path().join("take-3.wav"),
                dir.path().join("take-4.wav")
            ]
        );
        assert_eq!(std::fs::read(&taken).unwrap(), b"not ours");
        let mut back = [Vec::new(), Vec::new()];
        for (f, want) in stats.files.iter().zip([1000, 1000, 500]) {
            assert_eq!(f.frames, want);
            let d = decode_file(&f.path).unwrap();
            assert_eq!(d.frames(), want as usize);
            for (all, part) in back.iter_mut().zip(&d.channels) {
                all.extend_from_slice(part);
            }
        }
        for (got, want) in back.iter().zip([&left, &right]) {
            let err = got
                .iter()
                .zip(want.iter())
                .map(|(a, b)| (a - b).abs())
                .fold(0.0, f32::max);
            assert!(err < 1e-4, "max error {err}");
        }
    }
}
