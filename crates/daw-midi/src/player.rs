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
//!   chase on, restarts the notes that would be sounding at the new
//!   position had playback run up to it.
//! - **Recording.** While armed and playing, input notes and raw MIDI are
//!   copied to a queue with their timeline position, along with markers for
//!   jumps and stops. [`crate::record::Recorder`] turns that into notes.
//! - **Input gate.** [`PlayerHandle::set_input`] decides which live input
//!   passes (none, or one channel or all). Live notes that the gate closes
//!   on are stopped, so nothing hangs.
//! - **Preview.** [`PlayerHandle::preview`] sounds one note for an editor
//!   (a key clicked in the piano roll, a note being dragged) whether or not
//!   the transport plays. It bypasses the input gate and is never recorded.
//!
//! - **Full output.** An event that does not fit in the output list is
//!   counted ([`PlayerHandle::take_event_overflows`]) and the player's
//!   state follows what went out: a note-off or sustain-pedal lift is sent
//!   first thing in the next block, and a sequence note starts then, a
//!   chased one included.
//!   Nothing is left sounding for good.
//!
//! `process` never allocates, locks or blocks.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering::Relaxed};

use daw_core::dispose::{Collector, Retirer, deferred_drop};
use daw_core::event::{Event, EventKind, EventList, NoteId, midi};
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

/// A note the editor wants to hear, see [`PlayerHandle::preview`].
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Preview {
    pub key: u8,
    pub channel: u8,
    /// 0 to 1.
    pub velocity: f32,
}

/// The preview request in one atomic word: bits 0-6 key, 8-11 channel,
/// 12-18 velocity (0 to 127), bit 19 "a note is wanted", and a request
/// count from bit 20 so asking for the same note again retriggers it.
fn pack_preview(serial: u32, preview: Option<Preview>) -> u32 {
    let note = preview.map_or(0, |p| {
        // At least 1: a note-on at velocity 0 is a note-off to instruments.
        let velocity = u32::from(midi::to_7bit(p.velocity).max(1));
        u32::from(p.key & 0x7F) | u32::from(p.channel & 0x0F) << 8 | velocity << 12 | 1 << 19
    });
    serial << 20 | note
}

fn unpack_preview(word: u32) -> Option<Preview> {
    (word & 1 << 19 != 0).then(|| Preview {
        key: (word & 0x7F) as u8,
        channel: (word >> 8 & 0x0F) as u8,
        velocity: (word >> 12 & 0x7F) as f32 / 127.0,
    })
}

#[derive(Debug, Default)]
struct Shared {
    armed: AtomicBool,
    chase: AtomicBool,
    capture_overflows: AtomicU64,
    event_overflows: AtomicU64,
    input_open: AtomicBool,
    input_channel: AtomicU8,
    end_live: AtomicBool,
    preview: AtomicU32,
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
        owed: Owed::default(),
        chase_again: false,
        preview_seen: 0,
        previewing: None,
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

    /// Sounds `note` from the next block on, ending the note previewed
    /// before (if any); `None` ends the preview. Asking for the note that
    /// is already sounding starts it again. Only the newest request per
    /// block is heard.
    pub fn preview(&self, note: Option<Preview>) {
        let serial = (self.shared.preview.load(Relaxed) >> 20).wrapping_add(1) & 0xFFF;
        self.shared
            .preview
            .store(pack_preview(serial, note), Relaxed);
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

    /// How many events did not fit in the player's output list, since the
    /// last call. Ends of notes among them were sent a block late, starts
    /// of sequence notes too, and other events were lost.
    pub fn take_event_overflows(&self) -> u64 {
        self.shared.event_overflows.swap(0, Relaxed)
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
    owed: Owed,
    /// The last chase ran out of room in the output list: the next block
    /// chases again at its own position.
    chase_again: bool,
    /// The last preview request acted on.
    preview_seen: u32,
    /// The preview note sounding: channel, key and note id.
    previewing: Option<(u8, u8, NoteId)>,
    shared: Arc<Shared>,
}

/// The channel and value of a sustain pedal (CC 64) message.
fn sustain(kind: &EventKind) -> Option<(u8, bool)> {
    let EventKind::Midi(raw) = *kind else {
        return None;
    };
    // Decoded first: instruments do not hear a malformed message either.
    match midi::decode_padded(raw)? {
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

/// Calls `f` with the channel and key of every note in `held` on a channel
/// `gate` closed, and forgets them.
fn close_held(held: &mut [u128; 16], gate: &InputGate, mut f: impl FnMut(u8, u8)) {
    for (channel, keys) in held.iter_mut().enumerate() {
        let channel = channel as u8;
        if *keys == 0 || channel_open(gate, channel) {
            continue;
        }
        for key in 0..128u8 {
            if *keys & (1 << key) != 0 {
                f(channel, key);
            }
        }
        *keys = 0;
    }
}

/// The end of the note on `key`; a `note_id` of -1 ends whatever sounds
/// there.
fn key_off(channel: u8, key: u8, note_id: NoteId) -> EventKind {
    EventKind::NoteOff {
        channel,
        key: Some(key),
        velocity: 0.0,
        note_id,
    }
}

/// The sustain pedal of `channel` going up.
fn pedal_up(channel: u8) -> EventKind {
    EventKind::Midi([0xB0 | channel, 64, 0])
}

/// Appends an event to `out`. A full list drops it (the engine sizes lists
/// generously), which `shared` counts. Returns whether it went out.
fn push(out: &mut EventList, shared: &Shared, frame: u32, kind: EventKind) -> bool {
    let sent = out.push(Event { frame, kind }).is_ok();
    if !sent {
        shared.event_overflows.fetch_add(1, Relaxed);
    }
    sent
}

/// Note-offs and sustain-pedal lifts that found the output list full. They
/// go out first thing in the next block, so a full list delays the end of
/// a note but never leaves it sounding.
///
/// Only the key is kept, so the late note-off ends whatever sounds on it:
/// when a preview and a sequence note share a key and the preview's end
/// was owed, the sequence note ends with it.
#[derive(Debug, Default)]
struct Owed {
    /// One bit per key, per channel, like `MidiPlayer::live`.
    notes: [u128; 16],
    /// One bit per channel, like `MidiPlayer::pedal`.
    pedals: u16,
}

impl Owed {
    /// Sends the note-off on `key`, or owes it when `out` is full.
    fn note_off(
        &mut self,
        out: &mut EventList,
        shared: &Shared,
        frame: u32,
        (channel, key): (u8, u8),
        note_id: NoteId,
    ) {
        if !push(out, shared, frame, key_off(channel, key, note_id)) {
            self.notes[usize::from(channel & 0x0F)] |= 1 << (key & 0x7F);
        }
    }

    /// Lifts the sustain pedal of `channel`, or owes that when `out` is
    /// full.
    fn lift_pedal(&mut self, out: &mut EventList, shared: &Shared, channel: u8) {
        if !push(out, shared, 0, pedal_up(channel)) {
            self.pedals |= 1 << (channel & 0x0F);
        }
    }

    /// Sends what earlier blocks owe, as far as `out` has room.
    fn settle(&mut self, out: &mut EventList) {
        if self.pedals == 0 && self.notes.iter().all(|&keys| keys == 0) {
            return;
        }
        for channel in 0..16u8 {
            let pedal = 1 << channel;
            if self.pedals & pedal != 0 && out.push(event(pedal_up(channel))).is_ok() {
                self.pedals &= !pedal;
            }
            let keys = &mut self.notes[usize::from(channel)];
            if *keys == 0 {
                continue;
            }
            for key in 0..128u8 {
                let bit = 1 << key;
                if *keys & bit != 0 && out.push(event(key_off(channel, key, -1))).is_ok() {
                    *keys &= !bit;
                }
            }
        }
    }
}

/// An event at the start of the block.
fn event(kind: EventKind) -> Event {
    Event { frame: 0, kind }
}

impl MidiPlayer {
    /// Ends a sounding note; see [`Owed::note_off`].
    fn note_off(&mut self, out: &mut EventList, frame: u32, a: Active) {
        self.owed
            .note_off(out, &self.shared, frame, (a.channel, a.key), a.note_id);
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
        let (owed, shared) = (&mut self.owed, &*self.shared);
        close_held(&mut self.live, gate, |channel, key| {
            owed.note_off(out, shared, 0, (channel, key), -1);
        });
        for channel in 0..16u8 {
            let bit = 1 << channel;
            if self.pedal & bit != 0 && !channel_open(gate, channel) {
                owed.lift_pedal(out, shared, channel);
                self.pedal &= !bit;
            }
        }
    }

    /// Passes on the live input `gate` lets through.
    fn pass_input(&mut self, gate: &InputGate, input: &EventList, out: &mut EventList) {
        for e in input.iter() {
            let (live, pedal) = (self.live, self.pedal);
            if self.admit(gate, &e.kind) && !push(out, &self.shared, e.frame, e.kind) {
                // The instrument never heard this event: what it would
                // have released is still down there.
                for (owed, (before, now)) in
                    self.owed.notes.iter_mut().zip(live.iter().zip(&self.live))
                {
                    *owed |= before & !now;
                }
                self.owed.pedals |= pedal & !self.pedal;
            }
        }
    }

    /// Follows a new preview request: ends the old note, starts the new.
    fn update_preview(&mut self, out: &mut EventList) {
        let word = self.shared.preview.load(Relaxed);
        if word == self.preview_seen {
            return;
        }
        if let Some((channel, key, note_id)) = self.previewing.take() {
            self.owed
                .note_off(out, &self.shared, 0, (channel, key), note_id);
        }
        if let Some(p) = unpack_preview(word) {
            let Some(note_id) = self.note_on(out, 0, (p.channel, p.key), p.velocity) else {
                // No room: the request stays unseen for the next block.
                return;
            };
            self.previewing = Some((p.channel, p.key, note_id));
        }
        self.preview_seen = word;
    }

    fn all_off(&mut self, out: &mut EventList) {
        for i in 0..self.active.len() {
            self.note_off(out, 0, self.active[i]);
        }
        self.active.clear();
    }

    /// Sends a note-on with a fresh note id. `None` when `out` is full.
    fn note_on(
        &mut self,
        out: &mut EventList,
        frame: u32,
        (channel, key): (u8, u8),
        velocity: f32,
    ) -> Option<NoteId> {
        let note_id = self.next_note_id;
        let kind = EventKind::NoteOn {
            channel,
            key,
            velocity,
            note_id,
        };
        push(out, &self.shared, frame, kind).then(|| {
            self.next_note_id = note_id.wrapping_add(1) & NoteId::MAX;
            note_id
        })
    }

    /// Starts `n`, ending the note already on its key. `false` when `out`
    /// is full: `n` is not sounding, and the caller starts it later.
    fn start_note(&mut self, out: &mut EventList, frame: u32, n: &SeqNote) -> bool {
        // Retrigger: stop the same key on the same channel first.
        if let Some(i) = self
            .active
            .iter()
            .position(|a| a.key == n.key && a.channel == n.channel)
        {
            let a = self.active.swap_remove(i);
            self.note_off(out, frame, a);
        }
        if self.active.len() == MAX_ACTIVE
            && let Some(i) = (0..self.active.len()).min_by_key(|&i| self.active[i].end)
        {
            let a = self.active.swap_remove(i);
            self.note_off(out, frame, a);
        }
        let Some(note_id) = self.note_on(out, frame, (n.channel, n.key), n.velocity) else {
            return false;
        };
        self.active.push(Active {
            start: n.start,
            end: n.end.0,
            uid: n.uid,
            key: n.key,
            channel: n.channel,
            note_id,
        });
        true
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
                    self.note_off(out, 0, a);
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

    /// Starts the notes that playing up to `pos` would have left sounding
    /// and that are not sounding already. A note retriggers its key, so
    /// per channel and key only the last note to start before `pos` can
    /// sound, and only while it has not ended: an earlier note that still
    /// spans `pos` was cut off when the later one started, and is stopped
    /// here if it sounds (a note added behind the play position can make
    /// it so).
    ///
    /// When `out` fills up, the rest is chased in the next block.
    fn chase(&mut self, out: &mut EventList, pos: i64) {
        self.chase_again = false;
        let slot = |n: &SeqNote| (usize::from(n.channel & 0x0F), usize::from(n.key & 0x7F));
        let range = self.seq.sounding_range(pos);
        // How many notes of each channel and key are still to come: the
        // one that brings its count to 0 started last.
        let mut left = [[0u32; 128]; 16];
        for n in &self.seq.notes()[range.clone()] {
            let (channel, key) = slot(n);
            left[channel][key] += 1;
        }
        for i in range {
            let n = self.seq.notes()[i];
            let (channel, key) = slot(&n);
            left[channel][key] -= 1;
            let sounding = self
                .active
                .iter()
                .position(|a| a.uid == n.uid && a.start == n.start);
            if left[channel][key] != 0 {
                if let Some(at) = sounding {
                    let cut = self.active.swap_remove(at);
                    self.note_off(out, 0, cut);
                }
            } else if n.end.0 > pos && sounding.is_none() && !self.start_note(out, 0, &n) {
                self.chase_again = true;
                break;
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
        self.owed.settle(out);
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
        let closing = if ended { &closed } else { &gate };
        self.close_live(closing, out);
        self.pass_input(&gate, ctx.events_in, out);
        self.update_preview(out);

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
        } else if self.chase_again {
            self.chase(out, pos);
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
            close_held(&mut captured, closing, |channel, key| {
                self.capture(Captured::Event {
                    time: SampleTime(pos),
                    kind: key_off(channel, key, -1),
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
            // A note-off comes first unless a note starts before it.
            let next_on =
                next_on.filter(|on| next_off.is_none_or(|off| self.active[off].end > on.start.0));
            match (next_on, next_off) {
                (None, None) => break,
                (Some(on), _) => {
                    if !self.start_note(out, frame_of(on.start.0), &on) {
                        // No room: it starts with the next block.
                        break;
                    }
                    self.cursor += 1;
                }
                (None, Some(off)) => {
                    let a = self.active.swap_remove(off);
                    self.note_off(out, frame_of(a.end), a);
                }
            }
        }
        self.expected = Some(end);
    }

    fn reset(&mut self) {
        self.active.clear();
        self.owed = Owed::default();
        self.chase_again = false;
        // The instrument is reset too; a later request starts afresh.
        self.previewing = None;
        if let Some(at) = self.expected.take()
            && self.capturing
        {
            self.capture(Captured::Stop { at: SampleTime(at) });
        }
        self.capturing = false;
    }
}
