//! The MIDI clip player: a real-time [`Node`] that plays a [`Sequence`].
//!
//! The player sits between a track's event input and its instrument. It
//! passes input events straight through (so live playing and monitoring
//! work) and adds the notes of the sequence while the transport plays.
//!
//! - **Live edits.** [`PlayerHandle::set_sequence`] swaps in a new
//!   sequence without a graph rebuild. Notes that are sounding keep
//!   sounding if they still exist (with their new end), and stop if they
//!   were deleted. A note moved to another key, or added under the play
//!   position, starts at once when chase is on. Old sequences are freed on
//!   the handle's thread.
//! - **Seek and loop.** The engine splits blocks at the loop end, so a loop
//!   wrap or a locate shows up as a block whose position is not where the
//!   last one ended. The player then stops every sounding note and, with
//!   chase on, restarts notes that span the new position.
//! - **Recording.** While armed and playing, input notes and raw MIDI are
//!   copied to a queue with their timeline position, along with markers for
//!   jumps and stops. [`crate::record::Recorder`] turns that into notes.
//! - **Input gate.** [`PlayerHandle::set_input`] decides which live input
//!   passes (none, or one channel or all). Live notes that the gate closes
//!   on are stopped, so nothing hangs.
//!
//! `process` never allocates, locks or blocks.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering::Relaxed};

use daw_core::dispose::{Collector, Retirer, deferred_drop};
use daw_core::event::{Event, EventKind, EventList, NoteId};
use daw_core::node::{Node, PortLayout, ProcessContext};
use daw_core::queue::{Consumer, Producer, spsc};
use daw_core::time::SampleTime;

use crate::sequence::{SeqNote, Sequence};

/// Most notes the player sounds at once. Past this, the note that ends
/// soonest is stopped early.
pub const MAX_ACTIVE: usize = 256;

/// Sequences waiting to be picked up by the audio thread.
const SEQUENCE_QUEUE: usize = 8;

/// What the player hands to the recorder.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Captured {
    /// An input event at a timeline position.
    Event { time: SampleTime, kind: EventKind },
    /// The play position jumped (loop wrap or locate) from `from` to `to`.
    Jump { from: SampleTime, to: SampleTime },
    /// Playback or recording stopped at `at`.
    Stop { at: SampleTime },
}

/// Which live input a player passes on (and records).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct InputGate {
    /// Whether live input passes at all.
    pub open: bool,
    /// The one channel (0 to 15) that passes, or `None` for all.
    pub channel: Option<u8>,
}

impl Default for InputGate {
    fn default() -> Self {
        Self {
            open: true,
            channel: None,
        }
    }
}

impl InputGate {
    /// Every input event that a closed gate drops: channel messages on
    /// another channel, and everything but parameter changes when closed.
    fn passes(&self, kind: &EventKind) -> bool {
        let channel = match *kind {
            EventKind::ParamValue { .. } => return true,
            EventKind::NoteOn { channel, .. } | EventKind::NoteOff { channel, .. } => Some(channel),
            EventKind::Midi([status, ..]) if (0x80..0xF0).contains(&status) => Some(status & 0x0F),
            EventKind::Midi(_) => None,
        };
        self.open && (self.channel.is_none() || channel.is_none() || channel == self.channel)
    }
}

/// `InputGate::channel` in an atomic: this means every channel.
const ALL_CHANNELS: u8 = u8::MAX;

#[derive(Debug, Default)]
struct Shared {
    armed: AtomicBool,
    chase: AtomicBool,
    capture_overflows: AtomicU64,
    input_open: AtomicBool,
    input_channel: AtomicU8,
    end_live: AtomicBool,
}

/// Creates a player and the handle that controls it. `capture_capacity`
/// is how many captured events can wait for the recorder.
pub fn midi_player(capture_capacity: usize) -> (MidiPlayer, PlayerHandle) {
    let (seq_tx, seq_rx) = spsc(SEQUENCE_QUEUE);
    // Every queued sequence plus the playing one can be waiting to be freed.
    let (retirer, garbage) = deferred_drop(SEQUENCE_QUEUE * 2 + 2);
    let (capture_tx, capture_rx) = spsc(capture_capacity.max(1));
    let shared = Arc::new(Shared::default());
    shared.chase.store(true, Relaxed);
    shared.input_open.store(true, Relaxed);
    shared.input_channel.store(ALL_CHANNELS, Relaxed);
    let player = MidiPlayer {
        seq: Box::new(Sequence::empty()),
        seq_rx,
        retirer,
        unretired: None,
        active: Vec::with_capacity(MAX_ACTIVE),
        cursor: 0,
        expected: None,
        capturing: false,
        next_note_id: 0,
        capture_tx,
        live: [0; 16],
        pedal: 0,
        captured: [0; 16],
        shared: shared.clone(),
    };
    let handle = PlayerHandle {
        seq_tx,
        garbage,
        capture_rx,
        shared,
    };
    (player, handle)
}

/// Controls a [`MidiPlayer`] from a normal thread.
#[derive(Debug)]
pub struct PlayerHandle {
    seq_tx: Producer<Box<Sequence>>,
    garbage: Collector<Box<Sequence>>,
    capture_rx: Consumer<Captured>,
    shared: Arc<Shared>,
}

impl PlayerHandle {
    /// Queues a new sequence. Fails (handing it back) when the player has
    /// not picked up the last few yet; retry after its next cycle.
    pub fn set_sequence(&mut self, seq: Sequence) -> Result<(), Sequence> {
        self.collect_garbage();
        self.seq_tx.push(Box::new(seq)).map_err(|b| *b)
    }

    /// Frees sequences the player is done with. Call on a timer.
    pub fn collect_garbage(&mut self) -> usize {
        self.garbage.collect()
    }

    /// Arms or disarms recording. Capture runs only while also playing.
    pub fn set_armed(&self, armed: bool) {
        self.shared.armed.store(armed, Relaxed);
    }

    pub fn is_armed(&self) -> bool {
        self.shared.armed.load(Relaxed)
    }

    /// Whether notes spanning a new play position start sounding at once
    /// (default on).
    pub fn set_chase(&self, chase: bool) {
        self.shared.chase.store(chase, Relaxed);
    }

    /// Ends every live note (and lifts the sustain pedal) at the next
    /// block, for when the input it came from is rerouted and its
    /// note-offs would never arrive.
    pub fn end_live(&self) {
        self.shared.end_live.store(true, Relaxed);
    }

    /// Which live input passes from the next block on.
    pub fn set_input(&self, gate: InputGate) {
        self.shared
            .input_channel
            .store(gate.channel.map_or(ALL_CHANNELS, |c| c & 0x0F), Relaxed);
        self.shared.input_open.store(gate.open, Relaxed);
    }

    pub fn input(&self) -> InputGate {
        let channel = self.shared.input_channel.load(Relaxed);
        InputGate {
            open: self.shared.input_open.load(Relaxed),
            channel: (channel != ALL_CHANNELS).then_some(channel),
        }
    }

    /// Takes everything captured so far.
    pub fn drain_captured(&mut self) -> impl Iterator<Item = Captured> + '_ {
        self.capture_rx.drain()
    }

    /// How many captured events were lost because the queue was full,
    /// since the last call.
    pub fn take_capture_overflows(&self) -> u64 {
        self.shared.capture_overflows.swap(0, Relaxed)
    }
}

#[derive(Copy, Clone, Debug)]
struct Active {
    start: SampleTime,
    end: i64,
    uid: u64,
    key: u8,
    channel: u8,
    note_id: NoteId,
}

/// Plays a [`Sequence`] into the event stream. See the module docs.
pub struct MidiPlayer {
    seq: Box<Sequence>,
    seq_rx: Consumer<Box<Sequence>>,
    retirer: Retirer<Box<Sequence>>,
    // A replaced sequence the retire queue had no room for yet.
    unretired: Option<Box<Sequence>>,
    active: Vec<Active>,
    // First note of `seq` not yet started.
    cursor: usize,
    // Where the next block starts if nothing jumps; `None` while stopped.
    expected: Option<i64>,
    capturing: bool,
    next_note_id: NoteId,
    capture_tx: Producer<Captured>,
    /// Live input notes passed on and not yet ended: one bit per key, per
    /// channel.
    live: [u128; 16],
    /// Channels whose sustain pedal live input holds down.
    pedal: u16,
    /// Live notes captured and not yet ended, like `live`.
    captured: [u128; 16],
    shared: Arc<Shared>,
}

/// The channel and value of a sustain pedal (CC 64) message.
fn sustain(kind: &EventKind) -> Option<(u8, bool)> {
    match *kind {
        EventKind::Midi([status, 64, value]) if status & 0xF0 == 0xB0 => {
            Some((status & 0x0F, value >= 64))
        }
        _ => None,
    }
}

/// Whether a live note event passes `gate`, given the notes in `held`
/// (one bit per key, per channel) that passed before; updates `held`. The
/// end of a note that passed always passes too.
fn admit_note(held: &mut [u128; 16], gate: &InputGate, kind: &EventKind) -> bool {
    match *kind {
        EventKind::NoteOn { channel, key, .. } => {
            let ok = gate.passes(kind);
            if ok {
                held[usize::from(channel & 0x0F)] |= 1 << (key & 0x7F);
            }
            ok
        }
        EventKind::NoteOff { channel, key, .. } => {
            let held = &mut held[usize::from(channel & 0x0F)];
            let was_held = match key {
                Some(k) => {
                    let bit = 1 << (k & 0x7F);
                    let was = *held & bit != 0;
                    *held &= !bit;
                    was
                }
                None => std::mem::take(held) != 0,
            };
            was_held || gate.passes(kind)
        }
        _ => gate.passes(kind),
    }
}

/// Whether `gate` still lets notes on `channel` through.
fn channel_open(gate: &InputGate, channel: u8) -> bool {
    gate.passes(&EventKind::NoteOn {
        channel,
        key: 0,
        velocity: 0.0,
        note_id: -1,
    })
}

/// Calls `f` with a note-off for every note in `held` on a channel `gate`
/// closed, and forgets them.
fn close_held(held: &mut [u128; 16], gate: &InputGate, mut f: impl FnMut(EventKind)) {
    for (channel, keys) in held.iter_mut().enumerate() {
        let channel = channel as u8;
        if *keys == 0 || channel_open(gate, channel) {
            continue;
        }
        for key in 0..128u8 {
            if *keys & (1 << key) != 0 {
                f(EventKind::NoteOff {
                    channel,
                    key: Some(key),
                    velocity: 0.0,
                    note_id: -1,
                });
            }
        }
        *keys = 0;
    }
}

fn push(out: &mut EventList, frame: u32, kind: EventKind) {
    // A full list drops the event; the engine sizes lists generously.
    let _ = out.push(Event { frame, kind });
}

impl MidiPlayer {
    fn note_off(out: &mut EventList, frame: u32, a: &Active) {
        push(
            out,
            frame,
            EventKind::NoteOff {
                channel: a.channel,
                key: Some(a.key),
                velocity: 0.0,
                note_id: a.note_id,
            },
        );
    }

    /// Whether a live input event passes `gate`, keeping track of the
    /// notes it starts and the sustain pedal.
    fn admit(&mut self, gate: &InputGate, kind: &EventKind) -> bool {
        let ok = admit_note(&mut self.live, gate, kind);
        if ok && let Some((channel, down)) = sustain(kind) {
            let bit = 1 << channel;
            if down {
                self.pedal |= bit;
            } else {
                self.pedal &= !bit;
            }
        }
        ok
    }

    /// Ends the live notes, and lifts the sustain pedal, on channels `gate`
    /// no longer lets through.
    fn close_live(&mut self, gate: &InputGate, out: &mut EventList) {
        close_held(&mut self.live, gate, |kind| push(out, 0, kind));
        for channel in 0..16u8 {
            let bit = 1 << channel;
            if self.pedal & bit != 0 && !channel_open(gate, channel) {
                push(out, 0, EventKind::Midi([0xB0 | channel, 64, 0]));
                self.pedal &= !bit;
            }
        }
    }

    fn all_off(&mut self, out: &mut EventList) {
        for a in &self.active {
            Self::note_off(out, 0, a);
        }
        self.active.clear();
    }

    fn start_note(&mut self, out: &mut EventList, frame: u32, n: &SeqNote) {
        // Retrigger: stop the same key on the same channel first.
        if let Some(i) = self
            .active
            .iter()
            .position(|a| a.key == n.key && a.channel == n.channel)
        {
            let a = self.active.swap_remove(i);
            Self::note_off(out, frame, &a);
        }
        if self.active.len() == MAX_ACTIVE
            && let Some(i) = (0..self.active.len()).min_by_key(|&i| self.active[i].end)
        {
            let a = self.active.swap_remove(i);
            Self::note_off(out, frame, &a);
        }
        let note_id = self.next_note_id;
        self.next_note_id = self.next_note_id.wrapping_add(1) & NoteId::MAX;
        push(
            out,
            frame,
            EventKind::NoteOn {
                channel: n.channel,
                key: n.key,
                velocity: n.velocity,
                note_id,
            },
        );
        self.active.push(Active {
            start: n.start,
            end: n.end.0,
            uid: n.uid,
            key: n.key,
            channel: n.channel,
            note_id,
        });
    }

    fn retire(&mut self, old: Box<Sequence>) {
        if let Err(old) = self.retirer.retire(old) {
            // Only reached when the handle stops collecting; keep the one
            // and drop older ones as a last resort.
            if let Some(prev) = self.unretired.replace(old) {
                drop(prev);
            }
        }
    }

    /// Picks up the newest queued sequence and reconciles sounding notes.
    fn receive(&mut self, out: &mut EventList) {
        if let Some(old) = self.unretired.take() {
            self.retire(old);
            if self.unretired.is_some() {
                return;
            }
        }
        let mut newest = None;
        while let Some(seq) = self.seq_rx.pop() {
            if let Some(skipped) = newest.replace(seq) {
                self.retire(skipped);
            }
        }
        let Some(seq) = newest else { return };
        let old = std::mem::replace(&mut self.seq, seq);
        self.retire(old);
        let now = self.expected.unwrap_or(i64::MIN);
        let mut i = 0;
        while i < self.active.len() {
            let a = self.active[i];
            match self.seq.find(a.start, a.uid) {
                // A changed key or channel is a different sound: stop it
                // and let the chase below start the new one.
                Some(n) if n.end.0 > now && n.key == a.key && n.channel == a.channel => {
                    self.active[i].end = n.end.0;
                    i += 1;
                }
                _ => {
                    Self::note_off(out, 0, &a);
                    self.active.swap_remove(i);
                }
            }
        }
        self.cursor = match self.expected {
            Some(pos) => {
                // Edited notes, and notes added under the play position.
                if self.shared.chase.load(Relaxed) {
                    self.chase(out, pos);
                }
                self.seq.first_at_or_after(pos)
            }
            None => 0,
        };
    }

    fn capture(&mut self, c: Captured) {
        if self.capture_tx.push(c).is_err() {
            self.shared.capture_overflows.fetch_add(1, Relaxed);
        }
    }

    /// Starts every note that spans `pos` and is not already sounding.
    fn chase(&mut self, out: &mut EventList, pos: i64) {
        for i in self.seq.sounding_range(pos) {
            let n = self.seq.notes()[i];
            if n.end.0 > pos
                && !self
                    .active
                    .iter()
                    .any(|a| a.uid == n.uid && a.start == n.start)
            {
                self.start_note(out, 0, &n);
            }
        }
    }
}

impl Node for MidiPlayer {
    fn ports(&self) -> PortLayout {
        PortLayout {
            audio_in: 0,
            audio_out: 0,
            event_in: true,
            event_out: true,
        }
    }

    fn prepare(&mut self, _sample_rate: f64, _max_frames: u32) {}

    fn process(&mut self, ctx: &mut ProcessContext) {
        let out = &mut *ctx.events_out;
        self.receive(out);
        let gate = InputGate {
            open: self.shared.input_open.load(Relaxed),
            channel: Some(self.shared.input_channel.load(Relaxed)).filter(|&c| c != ALL_CHANNELS),
        };
        // Live input moved to other sources: their note-offs won't come.
        let ended = self.shared.end_live.swap(false, Relaxed);
        let closed = InputGate {
            open: false,
            channel: None,
        };
        self.close_live(if ended { &closed } else { &gate }, out);
        for e in ctx.events_in.iter() {
            if self.admit(&gate, &e.kind) {
                let _ = out.push(*e);
            }
        }

        let t = ctx.transport;
        let armed = self.shared.armed.load(Relaxed);
        if !t.playing {
            if let Some(at) = self.expected.take() {
                self.all_off(out);
                if self.capturing {
                    self.capture(Captured::Stop { at: SampleTime(at) });
                }
            }
            self.capturing = false;
            return;
        }

        let pos = t.position.0;
        if self.expected != Some(pos) {
            if let Some(from) = self.expected {
                self.all_off(out);
                if self.capturing {
                    self.capture(Captured::Jump {
                        from: SampleTime(from),
                        to: SampleTime(pos),
                    });
                }
            }
            if self.shared.chase.load(Relaxed) {
                self.chase(out, pos);
            }
            self.cursor = self.seq.first_at_or_after(pos);
        }
        if self.capturing && !armed {
            self.capture(Captured::Stop {
                at: SampleTime(pos),
            });
        }
        if !self.capturing {
            self.captured = [0; 16];
        }
        self.capturing = armed;

        if armed {
            // What reaches the instrument is what gets recorded: notes the
            // gate let start also end in the take, even if it closed since.
            let mut captured = self.captured;
            close_held(&mut captured, if ended { &closed } else { &gate }, |kind| {
                self.capture(Captured::Event {
                    time: SampleTime(pos),
                    kind,
                })
            });
            for e in ctx.events_in.iter() {
                if matches!(
                    e.kind,
                    EventKind::NoteOn { .. } | EventKind::NoteOff { .. } | EventKind::Midi(_)
                ) && admit_note(&mut captured, &gate, &e.kind)
                {
                    self.capture(Captured::Event {
                        time: SampleTime(pos + e.frame as i64),
                        kind: e.kind,
                    });
                }
            }
            self.captured = captured;
        }

        let end = pos + ctx.frames as i64;
        let frame_of = |time: i64| (time - pos).clamp(0, ctx.frames as i64 - 1) as u32;
        loop {
            let next_on = self
                .seq
                .notes()
                .get(self.cursor)
                .filter(|n| n.start.0 < end)
                .copied();
            let next_off = (0..self.active.len())
                .filter(|&i| self.active[i].end < end)
                .min_by_key(|&i| self.active[i].end);
            match (next_on, next_off) {
                (None, None) => break,
                (Some(on), Some(off)) if self.active[off].end > on.start.0 => {
                    self.cursor += 1;
                    self.start_note(out, frame_of(on.start.0), &on);
                }
                (Some(on), None) => {
                    self.cursor += 1;
                    self.start_note(out, frame_of(on.start.0), &on);
                }
                (_, Some(off)) => {
                    let a = self.active.swap_remove(off);
                    Self::note_off(out, frame_of(a.end), &a);
                }
            }
        }
        self.expected = Some(end);
    }

    fn reset(&mut self) {
        self.active.clear();
        if let Some(at) = self.expected.take()
            && self.capturing
        {
            self.capture(Captured::Stop { at: SampleTime(at) });
        }
        self.capturing = false;
    }
}
