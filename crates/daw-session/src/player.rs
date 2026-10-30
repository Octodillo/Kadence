//! Audio clip playback on the real-time thread.
//!
//! Each audio track gets one [`AudioClipPlayer`] node. Its clips arrive as a
//! [`ClipSet`] through a lock-free queue, so moving, trimming or adding a clip
//! never recompiles the graph: the [`PlayerHandle`] on the normal thread
//! builds a new set and the node swaps it in at the start of its next block.
//! Replaced sets go back through a deferred-drop queue to be freed off the RT
//! thread.
//!
//! Short files are played from memory. Long files are streamed through
//! `daw-media`'s [`DiskStreamer`], with two readers per clip: the main one
//! plays, and a cue reader waits at the point the clip is entered again after
//! a loop wrap. At the wrap the two swap, so looping never waits on the disk.
//! The same look-ahead cues a streamed clip shortly before it starts.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};

use daw_core::dispose::{Collector, Retirer, deferred_drop};
use daw_core::node::{Node, PortLayout, ProcessContext};
use daw_core::queue::{Consumer, Producer, spsc};
use daw_core::time::SampleTime;
use daw_media::{AudioData, ClipReader, DiskStreamer, MediaError, ReadStatus};
use daw_model::{ClipId, FadeShape};

/// Output channels of every player (tracks are stereo).
pub const PLAYER_CHANNELS: u16 = 2;

/// Length of the fade applied at clip edges so cuts don't click.
const EDGE_FADE: i64 = 64;

/// How far ahead a streamed clip is cued before it starts, in seconds.
const LOOK_AHEAD_SECONDS: f64 = 0.5;

/// How long an offline player waits for the disk on one read before it gives
/// up and plays silence.
const OFFLINE_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// Marker for "this reader is not positioned anywhere known".
const NOWHERE: i64 = i64::MIN;

/// The loop range, shared between the session and every player so the
/// players can prepare for the wrap. `end <= start` means no loop.
#[derive(Debug, Default)]
pub struct LoopPoints {
    start: AtomicI64,
    end: AtomicI64,
}

impl LoopPoints {
    pub fn set(&self, range: Option<(SampleTime, SampleTime)>) {
        let (s, e) = range.map_or((0, 0), |(s, e)| (s.0, e.0));
        // Clear the end first so a reader never sees a new start with an old end
        // that makes a bogus range for one block.
        self.end.store(0, Ordering::Relaxed);
        self.start.store(s, Ordering::Relaxed);
        self.end.store(e, Ordering::Release);
    }

    pub fn get(&self) -> Option<(i64, i64)> {
        let e = self.end.load(Ordering::Acquire);
        let s = self.start.load(Ordering::Relaxed);
        (e > s).then_some((s, e))
    }
}

/// Two readers on one file: see the module docs.
pub struct Streams {
    main: ClipReader,
    cue: ClipReader,
    /// Timeline position each reader will deliver next, or `NOWHERE`.
    main_next: i64,
    cue_next: i64,
}

impl Streams {
    pub fn open(streamer: &DiskStreamer, path: &std::path::Path) -> Result<Self, MediaError> {
        Ok(Self {
            main: streamer.open(path)?,
            cue: streamer.open(path)?,
            main_next: NOWHERE,
            cue_next: NOWHERE,
        })
    }

    /// Reads that came up short because the disk thread fell behind.
    pub fn underruns(&self) -> u64 {
        self.main.underruns() + self.cue.underruns()
    }
}

/// Where a clip's audio comes from.
pub enum ClipSource {
    /// Decoded and resampled to the engine rate.
    Memory(Arc<AudioData>),
    /// Streamed from disk. `None` takes the readers of the clip at
    /// [`ClipSet`]'s inherit index in the previous set.
    Stream(Option<Box<Streams>>),
    /// The file could not be read; the clip is silent.
    Missing,
}

/// One clip as the player sees it, in engine samples.
pub struct PlayClip {
    pub id: ClipId,
    pub path: PathBuf,
    /// Timeline span `[start, end)`.
    pub start: i64,
    pub end: i64,
    /// File frame (at the engine rate) played at `start`.
    pub file_start: i64,
    /// Linear gain.
    pub gain: f32,
    /// Whether to declick each edge. Off where the clip runs straight on
    /// into the next one, as after a split.
    pub fade_in: bool,
    pub fade_out: bool,
    /// The user's fades: length in samples and shape. Lengths are already
    /// shortened to fit the clip together.
    pub user_fade_in: (i64, FadeShape),
    pub user_fade_out: (i64, FadeShape),
    pub source: ClipSource,
}

impl PlayClip {
    fn file_pos(&self, timeline: i64) -> i64 {
        self.file_start + (timeline - self.start)
    }

    fn envelope(&self) -> Envelope {
        Envelope {
            start: self.start,
            end: self.end,
            gain: self.gain,
            declick_in: self.fade_in,
            declick_out: self.fade_out,
            fade_in: self.user_fade_in,
            fade_out: self.user_fade_out,
        }
    }
}

/// A clip's gain over time: its level, the user's fades and the short
/// declick at each edge.
#[derive(Copy, Clone)]
struct Envelope {
    start: i64,
    end: i64,
    gain: f32,
    declick_in: bool,
    declick_out: bool,
    fade_in: (i64, FadeShape),
    fade_out: (i64, FadeShape),
}

impl Envelope {
    /// Whether the whole span `[from, to)` plays at the plain clip gain.
    fn flat(&self, from: i64, to: i64) -> bool {
        let lead = if self.declick_in { EDGE_FADE } else { 0 }.max(self.fade_in.0);
        let tail = if self.declick_out { EDGE_FADE } else { 0 }.max(self.fade_out.0);
        from - self.start >= lead && self.end - to >= tail
    }

    /// Gain at `timeline`.
    fn at(&self, timeline: i64) -> f32 {
        let from_start = timeline - self.start;
        let to_end = self.end - 1 - timeline;
        let mut g = self.gain;
        let declick = |d: i64| (d.max(0) as f32 + 0.5) / EDGE_FADE as f32;
        if self.declick_in && from_start < EDGE_FADE {
            g *= declick(from_start);
        }
        if self.declick_out && to_end < EDGE_FADE {
            g *= declick(to_end);
        }
        let (len, shape) = self.fade_in;
        if from_start < len {
            g *= shape.gain((from_start.max(0) as f32 + 0.5) / len as f32);
        }
        let (len, shape) = self.fade_out;
        if to_end < len {
            g *= shape.gain((to_end.max(0) as f32 + 0.5) / len as f32);
        }
        g
    }
}

/// Everything one player plays.
#[derive(Default)]
pub struct ClipSet {
    pub clips: Vec<PlayClip>,
    /// For each clip, the index of the same clip in the previous set, whose
    /// stream readers it takes over.
    inherit: Vec<Option<usize>>,
}

impl ClipSet {
    pub fn len(&self) -> usize {
        self.clips.len()
    }

    pub fn is_empty(&self) -> bool {
        self.clips.is_empty()
    }
}

/// A render to a file that players take part in: see [`player`].
#[derive(Debug, Default)]
pub struct Offline {
    /// Set to stop waiting for the disk (the render was cancelled).
    pub stop: AtomicBool,
    /// Reads that gave up waiting and played silence.
    pub dropouts: AtomicU64,
}

/// Creates a player node and the handle that feeds it. An `offline` player waits
/// for the disk instead of playing silence when a streamed clip's audio isn't
/// buffered yet, so a render gets every sample; it must not run on a real-time
/// thread.
pub fn player(
    loop_points: Arc<LoopPoints>,
    offline: Option<Arc<Offline>>,
) -> (AudioClipPlayer, PlayerHandle) {
    let (tx, rx) = spsc(8);
    let (retirer, garbage) = deferred_drop(8);
    let node = AudioClipPlayer {
        clips: Box::default(),
        incoming: rx,
        retirer,
        retired: None,
        loop_points,
        scratch: Vec::new(),
        max_frames: 0,
        look_ahead: 24_000,
        expected: None,
        parked_at: NOWHERE,
        offline,
    };
    let handle = PlayerHandle {
        tx,
        garbage,
        sent: Vec::new(),
    };
    (node, handle)
}

/// The normal-thread side of a player.
pub struct PlayerHandle {
    tx: Producer<Box<ClipSet>>,
    garbage: Collector<Box<ClipSet>>,
    /// Ids and stream paths of the last set sent, in order.
    sent: Vec<(ClipId, Option<PathBuf>)>,
}

impl PlayerHandle {
    /// Whether a clip with this id and path was streamed in the last set, so
    /// the new set can reuse its readers instead of opening the file again.
    pub fn has_stream(&self, id: ClipId, path: &std::path::Path) -> bool {
        self.sent
            .iter()
            .any(|(i, p)| *i == id && p.as_deref() == Some(path))
    }

    /// Sends a new set of clips. Clips whose source is `Stream(None)` take
    /// over the readers of the same clip in the previous set (check with
    /// [`has_stream`](Self::has_stream) first). On a full queue the set is
    /// given back.
    pub fn send(&mut self, mut set: ClipSet) -> Result<(), ClipSet> {
        self.collect_garbage();
        set.inherit = set
            .clips
            .iter()
            .map(|c| match c.source {
                ClipSource::Stream(None) => self
                    .sent
                    .iter()
                    .position(|(i, p)| *i == c.id && p.as_deref() == Some(&c.path)),
                _ => None,
            })
            .collect();
        let sent = set
            .clips
            .iter()
            .map(|c| {
                let streamed = matches!(c.source, ClipSource::Stream(_));
                (c.id, streamed.then(|| c.path.clone()))
            })
            .collect();
        match self.tx.push(Box::new(set)) {
            Ok(()) => {
                self.sent = sent;
                Ok(())
            }
            Err(set) => Err(*set),
        }
    }

    /// Frees sets the node has replaced.
    pub fn collect_garbage(&mut self) -> usize {
        self.garbage.collect()
    }
}

/// Plays a track's audio clips. See the module docs.
pub struct AudioClipPlayer {
    clips: Box<ClipSet>,
    incoming: Consumer<Box<ClipSet>>,
    retirer: Retirer<Box<ClipSet>>,
    retired: Option<Box<ClipSet>>,
    loop_points: Arc<LoopPoints>,
    /// Two channels of `max_frames` for stream reads.
    scratch: Vec<f32>,
    max_frames: usize,
    look_ahead: i64,
    /// Where the next block starts if playback runs on without a jump.
    expected: Option<i64>,
    /// Position streams were cued for while stopped.
    parked_at: i64,
    /// See [`player`].
    offline: Option<Arc<Offline>>,
}

impl AudioClipPlayer {
    fn drain_updates(&mut self) {
        if let Some(old) = self.retired.take()
            && let Err(old) = self.retirer.retire(old)
        {
            // Keep the incoming set queued until the old one can go back.
            self.retired = Some(old);
            return;
        }
        while let Some(mut set) = self.incoming.pop() {
            for (i, from) in set.inherit.iter().enumerate() {
                let Some(from) = *from else { continue };
                let Some(old) = self.clips.clips.get_mut(from) else {
                    continue;
                };
                let new = &mut set.clips[i];
                // The readers' markers are timeline positions; they only
                // still hold if the timeline maps to the same file frames.
                let same_mapping = old.start - old.file_start == new.start - new.file_start;
                if let (ClipSource::Stream(dst), ClipSource::Stream(src)) =
                    (&mut new.source, &mut old.source)
                {
                    *dst = src.take();
                    if let Some(s) = dst
                        && !same_mapping
                    {
                        s.main_next = NOWHERE;
                        s.cue_next = NOWHERE;
                    }
                }
            }
            let old = std::mem::replace(&mut self.clips, set);
            // New or moved clips need cueing even if the transport is parked
            // where it was.
            self.parked_at = NOWHERE;
            if let Err(old) = self.retirer.retire(old) {
                self.retired = Some(old);
                return;
            }
        }
    }

    /// Positions every streamed clip for playback starting at `pos`.
    fn park(&mut self, pos: i64) {
        for c in &mut self.clips.clips {
            let target = pos.max(c.start);
            let file = c.file_pos(target);
            if let ClipSource::Stream(Some(s)) = &mut c.source
                && pos < c.end
                && target - pos <= self.look_ahead
                && s.main_next != target
            {
                s.main.cue(SampleTime(file));
                s.main_next = target;
            }
            if let ClipSource::Stream(Some(s)) = &mut c.source {
                s.main.is_ready();
                s.cue.is_ready();
            }
        }
    }

    /// Keeps cue readers waiting where each clip is entered after a wrap,
    /// and cues main readers for clips that start soon.
    fn prepare_ahead(&mut self, next: i64) {
        let lp = self.loop_points.get();
        for c in &mut self.clips.clips {
            let ClipSource::Stream(Some(s)) = &mut c.source else {
                continue;
            };
            // A reader that isn't being read never throws away the data from
            // before its last cue, and that stale data fills its ring so the
            // disk thread can't refill it. Polling drops it.
            s.cue.is_ready();
            if s.main_next != next {
                s.main.is_ready();
            }
            if let Some((ls, le)) = lp
                && c.start < le
                && c.end > ls
            {
                let entry = c.start.max(ls);
                if s.cue_next != entry {
                    s.cue.cue(SampleTime(c.file_start + (entry - c.start)));
                    s.cue_next = entry;
                }
            }
            if c.start > next && c.start - next <= self.look_ahead && s.main_next != c.start {
                s.main.cue(SampleTime(c.file_start));
                s.main_next = c.start;
            }
        }
    }
}

impl Node for AudioClipPlayer {
    fn ports(&self) -> PortLayout {
        PortLayout {
            audio_out: PLAYER_CHANNELS,
            ..Default::default()
        }
    }

    fn prepare(&mut self, sample_rate: f64, max_frames: u32) {
        self.max_frames = max_frames as usize;
        self.scratch = vec![0.0; 2 * self.max_frames];
        self.look_ahead = (sample_rate * LOOK_AHEAD_SECONDS) as i64;
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        self.drain_updates();
        let t = ctx.transport;
        let n = (ctx.frames as usize).min(self.max_frames);
        let pos = t.position.0;
        if !t.playing {
            if pos != self.parked_at {
                self.park(pos);
                self.parked_at = pos;
            }
            self.expected = None;
            return;
        }
        self.parked_at = NOWHERE;

        let wrapped = match (self.expected, self.loop_points.get()) {
            (Some(exp), Some((ls, le))) => exp == le && pos == ls,
            _ => false,
        };
        let end = pos + n as i64;
        let (sl, sr) = self.scratch.split_at_mut(self.max_frames);
        for c in &mut self.clips.clips {
            if wrapped
                && let ClipSource::Stream(Some(s)) = &mut c.source
                && s.cue_next == c.start.max(pos)
            {
                std::mem::swap(&mut s.main, &mut s.cue);
                std::mem::swap(&mut s.main_next, &mut s.cue_next);
            }
            let from = c.start.max(pos);
            let to = c.end.min(end);
            if from >= to {
                continue;
            }
            let at = (from - pos) as usize;
            let env = c.envelope();
            let flat = env.flat(from, to);
            let gain_at = |t: i64| if flat { env.gain } else { env.at(t) };
            let len = (to - from) as usize;
            match &mut c.source {
                ClipSource::Memory(data) => {
                    let f0 = c.file_start + (from - c.start);
                    for (ch, out) in ctx.audio_out.iter_mut().enumerate() {
                        let Some(src) = data
                            .channels
                            .get(ch.min(data.channels.len().saturating_sub(1)))
                        else {
                            continue;
                        };
                        for i in 0..len {
                            let f = f0 + i as i64;
                            if f >= 0
                                && let Some(x) = src.get(f as usize)
                            {
                                out[at + i] += x * gain_at(from + i as i64);
                            }
                        }
                    }
                }
                ClipSource::Stream(Some(s)) => {
                    let file = c.file_start + (from - c.start);
                    let mut bufs = [&mut sl[..len], &mut sr[..len]];
                    if let Some(offline) = &self.offline {
                        let status = s.main.read_blocking(
                            SampleTime(file),
                            &mut bufs,
                            OFFLINE_WAIT,
                            &offline.stop,
                        );
                        if status != ReadStatus::Ok {
                            offline.dropouts.fetch_add(1, Ordering::Relaxed);
                        }
                    } else {
                        s.main.read(SampleTime(file), &mut bufs);
                    }
                    s.main_next = to;
                    for (ch, out) in ctx.audio_out.iter_mut().enumerate().take(2) {
                        for i in 0..len {
                            out[at + i] += bufs[ch][i] * gain_at(from + i as i64);
                        }
                    }
                }
                ClipSource::Stream(None) | ClipSource::Missing => {}
            }
        }
        self.expected = Some(end);
        self.prepare_ahead(end);
    }

    fn reset(&mut self) {
        self.expected = None;
        self.parked_at = NOWHERE;
    }
}
