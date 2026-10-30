//! Audio input on audio tracks: monitoring and recording.
//!
//! Every audio track gets one [`InputTap`] node, fed by all of the graph's
//! hardware inputs. It picks the track's input channels, plays them into the
//! track when monitoring is on, and, once handed a [`RecordInput`], writes
//! them to it from the first block the transport plays. The node keeps the
//! transport position of the first frame it wrote, so the take can be put
//! on the timeline where it was played (see [`crate::record`]).
//!
//! Monitoring and the channel choice are atomics, so changing them doesn't
//! rebuild the graph. A recording input arrives through a lock-free queue
//! and goes back through another one, so it is never dropped on the audio
//! thread.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, Ordering};

use daw_core::node::{Node, PortLayout, ProcessContext};
use daw_core::queue::{Consumer, Producer, spsc};
use daw_media::RecordInput;
use daw_model::InputChannels;

/// Output channels of every tap (tracks are stereo).
const TAP_CHANNELS: u16 = 2;

/// Marker for "nothing recorded yet".
const NOWHERE: i64 = i64::MIN;

/// State shared between a tap and its handle.
#[derive(Debug)]
struct Shared {
    /// Play the input into the track.
    monitor: AtomicBool,
    /// The channels to monitor, packed by [`pack`].
    channels: AtomicU32,
    /// Transport position of the first frame written to the current
    /// recording input, or `NOWHERE`.
    started: AtomicI64,
    /// Set when the transport jumped (a loop wrap or a locate) while
    /// recording. The take goes on recording straight through, so what
    /// comes after the jump no longer lines up with the timeline.
    jumped: AtomicBool,
}

fn pack(c: InputChannels) -> u32 {
    (u32::from(c.first()) << 8) | u32::from(c.count())
}

fn unpack(v: u32) -> (usize, usize) {
    ((v >> 8) as usize, (v & 0xff) as usize)
}

enum Command {
    /// Record `channels` into the input from the next block the transport
    /// plays.
    Start(RecordInput, InputChannels),
    /// Stop recording and send the input back.
    Stop,
}

/// Makes a tap node and its handle.
pub(crate) fn input_tap(inputs: u16) -> (InputTap, TapHandle) {
    let shared = Arc::new(Shared {
        monitor: AtomicBool::new(false),
        channels: AtomicU32::new(pack(InputChannels::default())),
        started: AtomicI64::new(NOWHERE),
        jumped: AtomicBool::new(false),
    });
    let (cmd_tx, cmd_rx) = spsc(4);
    let (back_tx, back_rx) = spsc(16);
    (
        InputTap {
            inputs,
            shared: shared.clone(),
            commands: cmd_rx,
            back: back_tx,
            recording: None,
            next: None,
            pending: None,
        },
        TapHandle {
            shared,
            commands: cmd_tx,
            back: back_rx,
            recording: false,
        },
    )
}

/// The normal-thread side of an [`InputTap`].
pub struct TapHandle {
    shared: Arc<Shared>,
    commands: Producer<Command>,
    back: Consumer<RecordInput>,
    recording: bool,
}

impl TapHandle {
    /// Plays the input into the track, or stops.
    pub fn set_monitor(&self, on: bool) {
        self.shared.monitor.store(on, Ordering::Relaxed);
    }

    pub fn monitoring(&self) -> bool {
        self.shared.monitor.load(Ordering::Relaxed)
    }

    /// The inputs monitored. A recording keeps the channels it started with.
    pub fn set_channels(&self, channels: InputChannels) {
        self.shared
            .channels
            .store(pack(channels), Ordering::Relaxed);
    }

    /// Hands the tap a recording input. Recording starts with the next
    /// block the transport plays. Gives the input back if the queue is
    /// full.
    pub fn start(
        &mut self,
        input: RecordInput,
        channels: InputChannels,
    ) -> Result<(), RecordInput> {
        self.shared.started.store(NOWHERE, Ordering::Relaxed);
        self.shared.jumped.store(false, Ordering::Relaxed);
        self.commands
            .push(Command::Start(input, channels))
            .map_err(|c| match c {
                Command::Start(i, _) => i,
                Command::Stop => unreachable!(),
            })?;
        self.recording = true;
        Ok(())
    }

    /// Tells the tap to stop recording. Returns false if the queue was full
    /// (try again).
    pub fn stop(&mut self) -> bool {
        if !self.recording {
            return true;
        }
        if self.commands.push(Command::Stop).is_err() {
            return false;
        }
        self.recording = false;
        true
    }

    /// Whether a recording input was handed over and not stopped.
    pub fn is_recording(&self) -> bool {
        self.recording
    }

    /// Transport position, in samples, of the first frame recorded since
    /// the last [`start`](Self::start).
    pub fn started_at(&self) -> Option<i64> {
        let s = self.shared.started.load(Ordering::Acquire);
        (s != NOWHERE).then_some(s)
    }

    /// Whether the transport jumped during the take (see the field docs).
    pub fn jumped(&self) -> bool {
        self.shared.jumped.load(Ordering::Relaxed)
    }

    /// Where the current take started and whether it jumped, readable even
    /// after this handle and its node are gone (the track was deleted).
    pub fn progress(&self) -> TakeProgress {
        TakeProgress(self.shared.clone())
    }

    /// Frees recording inputs the tap has given back.
    pub fn collect_garbage(&mut self) {
        while self.back.pop().is_some() {}
    }
}

/// See [`TapHandle::progress`].
pub struct TakeProgress(Arc<Shared>);

impl TakeProgress {
    /// Transport position, in samples, of the take's first frame.
    pub fn started_at(&self) -> Option<i64> {
        let s = self.0.started.load(Ordering::Acquire);
        (s != NOWHERE).then_some(s)
    }

    pub fn jumped(&self) -> bool {
        self.0.jumped.load(Ordering::Relaxed)
    }
}

/// See the module docs.
pub struct InputTap {
    inputs: u16,
    shared: Arc<Shared>,
    commands: Consumer<Command>,
    back: Producer<RecordInput>,
    /// The input being recorded, with its first channel and channel count.
    recording: Option<(RecordInput, usize, usize)>,
    /// Transport position the next recorded frame should have; a different
    /// one means the transport jumped.
    next: Option<i64>,
    /// A command that waits for the previous input to be given back.
    pending: Option<Command>,
}

impl InputTap {
    fn drain_commands(&mut self) {
        // An input waiting for room in the back queue is retried every block.
        if matches!(self.recording, Some((_, _, 0))) {
            self.give_back();
        }
        loop {
            // A command waits while the old input can't be given back, so
            // replacing it never drops it here.
            if self.recording.as_ref().is_some_and(|r| r.2 == 0) {
                return;
            }
            let Some(cmd) = self.pending.take().or_else(|| self.commands.pop()) else {
                return;
            };
            self.give_back();
            if self.recording.is_some() {
                self.pending = Some(cmd);
                return;
            }
            if let Command::Start(input, ch) = cmd {
                self.recording = Some((input, ch.first() as usize, ch.count() as usize));
            }
        }
    }

    /// Sends the current input back to be dropped off the audio thread. If
    /// the queue is full it is kept, muted (count 0 writes nothing), and
    /// retried every block.
    fn give_back(&mut self) {
        if let Some((input, ..)) = self.recording.take() {
            self.next = None;
            if let Err(input) = self.back.push(input) {
                self.recording = Some((input, 0, 0));
            }
        }
    }
}

impl Node for InputTap {
    fn ports(&self) -> PortLayout {
        PortLayout {
            audio_in: self.inputs,
            audio_out: TAP_CHANNELS,
            event_in: false,
            event_out: false,
        }
    }

    fn prepare(&mut self, _sample_rate: f64, _max_frames: u32) {}

    fn process(&mut self, ctx: &mut ProcessContext) {
        self.drain_commands();
        let frames = ctx.frames as usize;
        let ins = ctx.audio_in;

        if let Some((input, first, count)) = &mut self.recording
            && *count > 0
            && ctx.transport.playing
            && *first + *count <= ins.len()
        {
            let at = ctx.transport.position.0;
            match self.next {
                None => self.shared.started.store(at, Ordering::Release),
                Some(n) if n != at => self.shared.jumped.store(true, Ordering::Relaxed),
                Some(_) => {}
            }
            self.next = Some(at + frames as i64);
            let chans: [&[f32]; 2] = [
                &ins[*first][..frames],
                &ins[(*first + *count - 1).min(ins.len() - 1)][..frames],
            ];
            input.write(&chans[..*count]);
        }

        let (first, count) = unpack(self.shared.channels.load(Ordering::Relaxed));
        let monitor = self.shared.monitor.load(Ordering::Relaxed) && first + count <= ins.len();
        for (ch, out) in ctx.audio_out.iter_mut().enumerate() {
            let out = &mut out[..frames];
            if monitor && count > 0 {
                // Mono goes to both sides; a pair goes left and right.
                let src = first + ch.min(count - 1);
                out.copy_from_slice(&ins[src][..frames]);
            } else {
                out.fill(0.0);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use daw_core::event::EventList;
    use daw_core::node::TransportState;
    use daw_core::time::SampleTime;
    use daw_media::{Recorder, WavFormat, decode_file};

    use super::*;

    fn run(tap: &mut InputTap, playing: bool, at: i64, ins: [&[f32]; 2]) -> [Vec<f32>; 2] {
        let frames = ins[0].len();
        let transport = TransportState {
            playing,
            position: SampleTime(at),
            sample_rate: 48_000.0,
            ..Default::default()
        };
        let (mut l, mut r) = (vec![9.0; frames], vec![9.0; frames]);
        let events = EventList::with_capacity(0);
        let mut events_out = EventList::with_capacity(0);
        let mut outs: [&mut [f32]; 2] = [&mut l, &mut r];
        let mut ctx = ProcessContext {
            frames: frames as u32,
            transport: &transport,
            audio_in: &ins,
            audio_out: &mut outs,
            events_in: &events,
            events_out: &mut events_out,
        };
        tap.process(&mut ctx);
        [l, r]
    }

    #[test]
    fn monitors_the_chosen_inputs() {
        let (mut tap, handle) = input_tap(2);
        let (a, b) = ([1.0f32; 4], [2.0f32; 4]);
        assert_eq!(run(&mut tap, false, 0, [&a, &b]), [[0.0; 4], [0.0; 4]]);
        handle.set_monitor(true);
        handle.set_channels(InputChannels::Mono(1));
        assert_eq!(run(&mut tap, false, 0, [&a, &b]), [[2.0; 4], [2.0; 4]]);
        handle.set_channels(InputChannels::Stereo(0));
        assert_eq!(run(&mut tap, false, 0, [&a, &b]), [[1.0; 4], [2.0; 4]]);
        // A pair past the last input is silent rather than out of bounds.
        handle.set_channels(InputChannels::Stereo(1));
        assert_eq!(run(&mut tap, false, 0, [&a, &b]), [[0.0; 4], [0.0; 4]]);
    }

    #[test]
    fn records_from_the_first_played_block_and_gives_the_input_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.wav");
        let (rec, input) = Recorder::start(&path, 1, 48_000, WavFormat::Float32, 1.0).unwrap();
        let (mut tap, mut handle) = input_tap(2);
        assert!(handle.start(input, InputChannels::Mono(1)).is_ok());
        let a = [0.5f32; 4];
        let b = [0.25f32, 0.5, 0.75, 1.0];
        // Stopped: nothing is written yet.
        run(&mut tap, false, 100, [&a, &b]);
        assert_eq!(handle.started_at(), None);
        run(&mut tap, true, 100, [&a, &b]);
        run(&mut tap, true, 104, [&a, &b]);
        assert_eq!(handle.started_at(), Some(100));
        assert!(!handle.jumped());
        run(&mut tap, true, 0, [&a, &b]);
        assert!(handle.jumped());
        assert!(handle.stop());
        run(&mut tap, true, 4, [&a, &b]);
        handle.collect_garbage();
        let stats = rec.finish().unwrap();
        assert_eq!(stats.frames, 12);
        let data = decode_file(&path).unwrap();
        assert_eq!(data.channels.len(), 1);
        assert_eq!(&data.channels[0][..4], &b);
    }
}
