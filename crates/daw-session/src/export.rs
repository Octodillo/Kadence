//! Rendering a project to an audio file, faster than real time.
//!
//! An export plays the project through its own [`Session`], built from the
//! same snapshot with the same code as playback: clip players, MIDI players,
//! devices and automation. The only difference is that its clip players wait
//! for the disk instead of playing silence, so a streamed clip never drops
//! out however fast the render runs. Nothing is shared with the session the
//! user hears, so playback carries on during an export.
//!
//! [`ExportJob`] runs [`export`] on a worker thread with progress and cancel;
//! [`render`] is the loop underneath, handing each block to a closure.
//!
//! Plugin devices play when the render is given a [`PluginLoader`]: the
//! export loads its own instances (in their own sandbox hosts), set to the
//! states stored in the snapshot.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;

use daw_core::event::EventList;
use daw_core::time::TickTime;
use daw_engine::EngineConfig;
use daw_media::{AudioWriter, FileFormat, MediaError};
use daw_model::{Project, ProjectSnapshot};
use daw_plugin_host::SandboxConfig;

use crate::player::Offline;
use crate::plugins::PluginLoader;
use crate::{Session, SessionConfig, SessionError, Warning};

/// Frames the render processes at a time.
const BLOCK: u32 = 512;

/// Every export is stereo, like the master.
pub const CHANNELS: usize = 2;

/// The longest tail an export may have, in seconds.
pub const MAX_TAIL_SECONDS: f64 = 3600.0;

/// Lowest and highest sample rate an export may use, in Hz.
pub const MIN_SAMPLE_RATE: u32 = 8_000;
pub const MAX_SAMPLE_RATE: u32 = 384_000;

/// What to render and how.
#[derive(Clone, Debug, PartialEq)]
pub struct RenderSettings {
    /// The part of the song to render, `[start, end)`.
    pub start: TickTime,
    pub end: TickTime,
    /// Seconds rendered after `end` with the transport stopped, so reverb
    /// and released notes ring out. Clips stop at `end`.
    pub tail_seconds: f64,
    pub sample_rate: u32,
}

/// What an export writes.
#[derive(Clone, Debug, PartialEq)]
pub struct ExportSettings {
    pub render: RenderSettings,
    pub format: FileFormat,
    /// Add dither when rounding to an integer format.
    pub dither: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    /// The range to render is empty (an empty project's whole song, say).
    #[error("nothing to export: the range is empty")]
    EmptyRange,
    #[error("unsupported sample rate {0} Hz")]
    SampleRate(u32),
    #[error("tail length {0} s is not a length")]
    Tail(f64),
    #[error(transparent)]
    Session(#[from] SessionError),
    #[error(transparent)]
    Write(#[from] MediaError),
    #[error("the export was cancelled")]
    Cancelled,
    /// Audio files could not be read fast enough: this many reads gave up
    /// waiting, so the render would have gaps. Nothing is written.
    #[error("the disk was too slow: {0} reads timed out")]
    DiskTooSlow(u64),
    /// Sandboxed plugins missed this many cycles, each a block of silence.
    #[error("plugins did not answer in time {0} times")]
    PluginTooSlow(u64),
    /// Device `0`'s plugin crashed during the render, so it was bypassed
    /// from there on.
    #[error("the plugin of {0} crashed")]
    PluginCrashed(String),
    /// A plugin changed its latency during the render, which would shift
    /// the audio after that point.
    #[error("a plugin changed its latency during the export")]
    LatencyChanged,
}

/// `config` adjusted for rendering offline: the render waits for a plugin
/// as long as it takes short of a hang (it runs faster than real time, so
/// the real-time deadline would play silence), and the host gets no
/// real-time priority, which belongs to playback.
pub fn offline_sandbox(mut config: SandboxConfig) -> SandboxConfig {
    config.process_timeout = Some(config.hang_timeout);
    config.rt_priority = None;
    config
}

/// Makes the plugin loader for an export, on the thread that renders: plugin
/// instances stay on the thread that loaded them.
pub type LoaderFactory = Box<dyn FnOnce() -> Option<Box<dyn PluginLoader>> + Send>;

/// What a finished export did.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ExportReport {
    /// Frames written.
    pub frames: u64,
    /// The file's sample rate.
    pub sample_rate: u32,
    /// Problems building the project, such as audio files that could not be
    /// read or plugins that would not load or crashed: the file was written
    /// without them.
    pub warnings: Vec<Warning>,
}

/// How far a render is, and the switch to cancel it. Shared between the
/// worker and whoever watches it.
#[derive(Debug, Default)]
pub struct Progress {
    done: AtomicU64,
    total: AtomicU64,
    cancelled: AtomicBool,
    /// Shared with the render's clip players, so a cancel also stops them
    /// waiting for the disk.
    offline: Arc<Offline>,
}

impl Progress {
    /// From 0 to 1; 0 until the render knows its length.
    pub fn fraction(&self) -> f64 {
        let total = self.total.load(Ordering::Relaxed);
        if total == 0 {
            return 0.0;
        }
        (self.done.load(Ordering::Relaxed) as f64 / total as f64).min(1.0)
    }

    /// Asks the render to stop at the end of its current block.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
        self.offline.stop.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }
}

/// Where the song ends: the end of its last clip. Zero for a song with no
/// clips.
pub fn song_end(project: &Project) -> TickTime {
    project
        .all_tracks()
        .flat_map(|t| t.clips.iter())
        .map(|c| TickTime(c.start.0 + c.length.0))
        .max()
        .unwrap_or_default()
}

/// Plays `snapshot` from `settings.start` to `settings.end` plus the tail
/// and gives each block of the master output to `sink`, one slice per
/// channel. Relative clip paths resolve against `root` (the project bundle).
/// Plugin devices load through `loader`; without one they are bypassed.
/// Returns the warnings from building and playing the project.
pub fn render(
    snapshot: &ProjectSnapshot,
    root: Option<PathBuf>,
    loader: Option<Box<dyn PluginLoader>>,
    settings: &RenderSettings,
    progress: &Progress,
    mut sink: impl FnMut(&[&[f32]]) -> Result<(), ExportError>,
) -> Result<Vec<Warning>, ExportError> {
    if !(MIN_SAMPLE_RATE..=MAX_SAMPLE_RATE).contains(&settings.sample_rate) {
        return Err(ExportError::SampleRate(settings.sample_rate));
    }
    if !(0.0..=MAX_TAIL_SECONDS).contains(&settings.tail_seconds) {
        return Err(ExportError::Tail(settings.tail_seconds));
    }
    if settings.end <= settings.start {
        return Err(ExportError::EmptyRange);
    }
    let rate = f64::from(settings.sample_rate);
    let (mut session, mut processor) = Session::new(SessionConfig {
        engine: EngineConfig {
            sample_rate: rate,
            max_frames: BLOCK,
            ..Default::default()
        },
        inputs: 0,
        outputs: CHANNELS as u16,
    });
    session.offline = Some(progress.offline.clone());
    session.set_root(root);
    if loader.is_some() {
        session.set_plugin_loader(loader);
    }
    if progress.is_cancelled() {
        return Err(ExportError::Cancelled);
    }
    // Loads every clip short enough to play from memory, and every plugin,
    // which takes a while.
    session.sync(snapshot)?;
    if progress.is_cancelled() {
        return Err(ExportError::Cancelled);
    }
    let mut warnings = session.warnings().to_vec();
    let start = session.ticks_to_samples(settings.start).0;
    let end = session.ticks_to_samples(settings.end).0;
    let body = u64::try_from(end - start).unwrap_or(0);
    if body == 0 {
        return Err(ExportError::EmptyRange);
    }
    let tail = (settings.tail_seconds * rate).round() as u64;
    session.locate(settings.start)?;
    session.play()?;

    let events = EventList::with_capacity(0);
    // Installs the graph and starts the transport, so its latency is known.
    processor.process(0, &[], &mut [], &events);
    let latency = u64::from(session.controller().latency());
    let total = body + tail;
    progress.total.store(total, Ordering::Relaxed);

    let block = BLOCK as usize;
    let mut bufs = vec![vec![0.0f32; block]; CHANNELS];
    // Timeline frames processed, and frames handed to `sink`.
    let mut ran = 0u64;
    let mut written = 0u64;
    while written < total {
        if progress.is_cancelled() {
            return Err(ExportError::Cancelled);
        }
        if ran == body {
            // Clips stop where the range ends; the tail is what rings on.
            session.stop()?;
        }
        // Stop exactly at the end of the range: commands apply between blocks.
        let limit = if ran < body { body - ran } else { u64::MAX };
        let n = (block as u64).min(limit).min(total + latency - ran) as usize;
        {
            let mut outs: Vec<&mut [f32]> = bufs.iter_mut().map(|b| &mut b[..n]).collect();
            processor.process(n, &[], &mut outs, &events);
        }
        // The first `latency` frames out are from before the range.
        let skip = latency.saturating_sub(ran).min(n as u64) as usize;
        ran += n as u64;
        if skip < n {
            let planes: Vec<&[f32]> = bufs.iter().map(|b| &b[skip..n]).collect();
            sink(&planes)?;
            written += (n - skip) as u64;
            progress.done.store(written, Ordering::Relaxed);
        }
        // Plugins' main-thread work. A crashed plugin would be bypassed from
        // here on, leaving the file without it.
        let report = session.tick();
        if let Some(e) = report.error {
            return Err(e.into());
        }
        for w in report.warnings {
            if let Warning::PluginCrashed { device, .. } = w {
                return Err(ExportError::PluginCrashed(device));
            }
            if !warnings.contains(&w) {
                warnings.push(w);
            }
        }
        if u64::from(session.controller().latency()) != latency {
            return Err(ExportError::LatencyChanged);
        }
    }
    let late = session.plugin_late_cycles();
    // The processor frees the graph; drop it before the session, as the
    // audio host does.
    drop(processor);
    drop(session);
    if progress.is_cancelled() {
        return Err(ExportError::Cancelled);
    }
    match (progress.offline.dropouts.load(Ordering::Relaxed), late) {
        (0, 0) => Ok(warnings),
        (0, n) => Err(ExportError::PluginTooSlow(n)),
        (n, _) => Err(ExportError::DiskTooSlow(n)),
    }
}

/// Renders `snapshot` into an audio file at `path`. The file appears only
/// once it is complete: a failed or cancelled export leaves nothing behind,
/// and an existing file at `path` untouched.
pub fn export(
    snapshot: &ProjectSnapshot,
    root: Option<PathBuf>,
    loader: Option<Box<dyn PluginLoader>>,
    path: &Path,
    settings: &ExportSettings,
    progress: &Progress,
) -> Result<ExportReport, ExportError> {
    let mut writer = AudioWriter::create(
        path,
        settings.format,
        CHANNELS,
        settings.render.sample_rate,
        settings.dither,
    )?;
    let warnings = render(
        snapshot,
        root,
        loader,
        &settings.render,
        progress,
        |planes| Ok(writer.write(planes)?),
    )?;
    let frames = writer.frames();
    if progress.is_cancelled() {
        return Err(ExportError::Cancelled);
    }
    writer.finish()?;
    Ok(ExportReport {
        frames,
        sample_rate: settings.render.sample_rate,
        warnings,
    })
}

/// An [`export`] running on its own thread.
pub struct ExportJob {
    progress: Arc<Progress>,
    path: PathBuf,
    thread: Option<JoinHandle<Result<ExportReport, ExportError>>>,
}

impl ExportJob {
    /// Starts exporting `snapshot` to `path`, loading plugins through the
    /// loader `loader` makes.
    pub fn start(
        snapshot: ProjectSnapshot,
        root: Option<PathBuf>,
        loader: Option<LoaderFactory>,
        path: PathBuf,
        settings: ExportSettings,
    ) -> std::io::Result<Self> {
        let progress = Arc::new(Progress::default());
        let thread = {
            let progress = progress.clone();
            let path = path.clone();
            std::thread::Builder::new()
                .name("daw-export".into())
                .spawn(move || {
                    let loader = loader.and_then(|make| make());
                    export(&snapshot, root, loader, &path, &settings, &progress)
                })?
        };
        Ok(Self {
            progress,
            path,
            thread: Some(thread),
        })
    }

    pub fn progress(&self) -> f64 {
        self.progress.fraction()
    }

    /// The file being written.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Asks the export to stop; [`wait`](Self::wait) then returns
    /// [`ExportError::Cancelled`] and no file is written.
    pub fn cancel(&self) {
        self.progress.cancel();
    }

    /// Whether the export has ended, so [`wait`](Self::wait) won't block.
    pub fn is_finished(&self) -> bool {
        self.thread.as_ref().is_none_or(JoinHandle::is_finished)
    }

    /// Waits for the export to end and returns how it went.
    pub fn wait(mut self) -> Result<ExportReport, ExportError> {
        let thread = self.thread.take().expect("only taken here");
        match thread.join() {
            Ok(r) => r,
            Err(panic) => std::panic::resume_unwind(panic),
        }
    }
}

impl Drop for ExportJob {
    /// An abandoned export is cancelled, and its thread waited for so the
    /// temporary file is gone when this returns.
    fn drop(&mut self) {
        if let Some(thread) = self.thread.take() {
            self.progress.cancel();
            let _ = thread.join();
        }
    }
}
