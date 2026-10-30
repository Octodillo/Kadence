//! MIDI recording: turns what the player captured into notes, then merges
//! the take into a clip.
//!
//! The flow is: arm the [`PlayerHandle`](crate::player::PlayerHandle),
//! feed [`PlayerHandle::drain_captured`](crate::player::PlayerHandle::drain_captured)
//! into a [`Recorder`] on a timer (its [`Recorder::notes`] and
//! [`Recorder::held`] let the UI draw the take as it grows), call
//! [`Recorder::finish`] when recording stops, convert with [`to_clip_notes`]
//! and plan the edit with [`merge_take`].
//!
//! A loop wrap or locate while keys are held ends those notes at the jump
//! and starts them again where playback continues, so a held chord carries
//! across the loop seam. Loop passes all land in one take, overdubbed.

use std::ops::Range;

use daw_core::event::{EventKind, NoteId};
use daw_core::time::{SampleTime, TempoMap, TickTime};

use crate::note::{EditNote, NoteData};
use crate::player::Captured;

/// A recorded note on the timeline, in samples.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct RecordedNote {
    pub start: SampleTime,
    pub end: SampleTime,
    pub key: u8,
    pub channel: u8,
    pub velocity: f32,
}

#[derive(Copy, Clone, Debug)]
struct Held {
    start: i64,
    velocity: f32,
    note_id: NoteId,
}

/// Pairs captured note-ons and note-offs into notes.
#[derive(Clone, Debug)]
pub struct Recorder {
    held: Vec<Option<Held>>,
    notes: Vec<RecordedNote>,
    last_time: i64,
}

impl Default for Recorder {
    fn default() -> Self {
        Self::new()
    }
}

enum Edge {
    On {
        channel: u8,
        key: u8,
        velocity: f32,
        note_id: NoteId,
    },
    Off {
        channel: u8,
        key: u8,
    },
    /// A wildcard note off: every key on the channel, or with an id of 0 or more, that note.
    OffAny {
        channel: u8,
        note_id: NoteId,
    },
}

fn edge(kind: &EventKind) -> Option<Edge> {
    match *kind {
        EventKind::NoteOn {
            channel,
            key,
            velocity,
            note_id,
        } if velocity > 0.0 => Some(Edge::On {
            channel,
            key,
            velocity: velocity.min(1.0),
            note_id,
        }),
        EventKind::NoteOn { channel, key, .. }
        | EventKind::NoteOff {
            channel,
            key: Some(key),
            ..
        } => Some(Edge::Off { channel, key }),
        EventKind::NoteOff {
            channel,
            key: None,
            note_id,
            ..
        } => Some(Edge::OffAny { channel, note_id }),
        EventKind::Midi([status, key, vel]) => {
            let channel = status & 0x0f;
            let key = key & 0x7f;
            match status & 0xf0 {
                0x90 if vel & 0x7f > 0 => Some(Edge::On {
                    channel,
                    key,
                    velocity: (vel & 0x7f) as f32 / 127.0,
                    note_id: -1,
                }),
                0x80 | 0x90 => Some(Edge::Off { channel, key }),
                _ => None,
            }
        }
        EventKind::ParamValue { .. } => None,
    }
}

impl Recorder {
    pub fn new() -> Self {
        Self {
            held: vec![None; 16 * 128],
            notes: Vec::new(),
            last_time: i64::MIN,
        }
    }

    fn slot(channel: u8, key: u8) -> Option<usize> {
        (channel < 16 && key < 128).then_some(channel as usize * 128 + key as usize)
    }

    fn close(&mut self, slot: usize, at: i64) {
        if let Some(h) = self.held[slot].take() {
            self.notes.push(RecordedNote {
                start: SampleTime(h.start),
                end: SampleTime(at.max(h.start + 1)),
                key: (slot % 128) as u8,
                channel: (slot / 128) as u8,
                velocity: h.velocity,
            });
        }
    }

    fn close_all(&mut self, at: i64) {
        for slot in 0..self.held.len() {
            self.close(slot, at);
        }
    }

    pub fn feed(&mut self, c: Captured) {
        match c {
            Captured::Event { time, kind } => {
                self.last_time = self.last_time.max(time.0);
                match edge(&kind) {
                    Some(Edge::On {
                        channel,
                        key,
                        velocity,
                        note_id,
                    }) => {
                        if let Some(slot) = Self::slot(channel, key) {
                            self.close(slot, time.0);
                            self.held[slot] = Some(Held {
                                start: time.0,
                                velocity,
                                note_id,
                            });
                        }
                    }
                    Some(Edge::Off { channel, key }) => {
                        if let Some(slot) = Self::slot(channel, key) {
                            self.close(slot, time.0);
                        }
                    }
                    Some(Edge::OffAny { channel, note_id }) if channel < 16 => {
                        let keys = channel as usize * 128..(channel as usize + 1) * 128;
                        for slot in keys {
                            if self.held[slot].is_some_and(|h| note_id < 0 || h.note_id == note_id)
                            {
                                self.close(slot, time.0);
                            }
                        }
                    }
                    Some(Edge::OffAny { .. }) => {}
                    None => {}
                }
            }
            Captured::Jump { from, to } => {
                for slot in 0..self.held.len() {
                    if let Some(h) = self.held[slot] {
                        self.close(slot, from.0);
                        self.held[slot] = Some(Held { start: to.0, ..h });
                    }
                }
                self.last_time = to.0;
            }
            Captured::Stop { at } => {
                self.close_all(at.0);
                self.last_time = at.0;
            }
        }
    }

    /// Finished notes so far, in the order they ended.
    pub fn notes(&self) -> &[RecordedNote] {
        &self.notes
    }

    /// Keys still held, as `(channel, key, start, velocity)`, for drawing
    /// notes that are still growing.
    pub fn held(&self) -> impl Iterator<Item = (u8, u8, SampleTime, f32)> + '_ {
        self.held.iter().enumerate().filter_map(|(slot, h)| {
            h.map(|h| {
                (
                    (slot / 128) as u8,
                    (slot % 128) as u8,
                    SampleTime(h.start),
                    h.velocity,
                )
            })
        })
    }

    /// Ends held keys at `at` (or at the last time seen) and returns the
    /// take sorted by start. The recorder is empty afterwards.
    pub fn finish(&mut self, at: Option<SampleTime>) -> Vec<RecordedNote> {
        let at = at.map_or(self.last_time, |t| t.0);
        self.close_all(at);
        self.last_time = i64::MIN;
        let mut notes = std::mem::take(&mut self.notes);
        notes.sort_by_key(|n| (n.start, n.key, n.channel));
        notes
    }
}

/// Converts recorded notes to ticks relative to `clip.start`. Notes
/// entirely outside `clip` (a timeline range) are dropped, and notes
/// crossing its edges are cut to fit.
pub fn to_clip_notes(
    notes: &[RecordedNote],
    tempo: &dyn TempoMap,
    sample_rate: f64,
    clip: Range<TickTime>,
) -> Vec<NoteData> {
    notes
        .iter()
        .filter_map(|n| {
            let s = tempo.samples_to_ticks(n.start, sample_rate).0;
            let e = tempo.samples_to_ticks(n.end, sample_rate).0;
            let start = s.max(clip.start.0);
            let end = e.min(clip.end.0);
            if end <= clip.start.0 || start >= clip.end.0 {
                return None;
            }
            Some(NoteData {
                start: TickTime(start - clip.start.0),
                length: TickTime((end - start).max(1)),
                key: n.key.min(127),
                channel: n.channel.min(15),
                velocity: n.velocity.clamp(0.0, 1.0) as f64,
            })
        })
        .collect()
}

/// How a take combines with the notes already in the clip.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum TakeMode {
    /// Keep the existing notes and add the take.
    #[default]
    Overdub,
    /// Remove existing notes that start inside the recorded range first.
    Replace,
}

/// The edit a take makes: notes to remove (indices into the existing
/// notes) and notes to add.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TakePlan {
    pub remove: Vec<usize>,
    pub add: Vec<NoteData>,
}

/// Plans merging `take` into a clip. `range` is the clip-relative span
/// that was recorded over, used by [`TakeMode::Replace`].
pub fn merge_take<N: EditNote>(
    existing: &[N],
    take: Vec<NoteData>,
    mode: TakeMode,
    range: Range<TickTime>,
) -> TakePlan {
    let remove = match mode {
        TakeMode::Overdub => Vec::new(),
        TakeMode::Replace => existing
            .iter()
            .enumerate()
            .filter(|(_, n)| range.contains(&n.data().start))
            .map(|(i, _)| i)
            .collect(),
    };
    TakePlan { remove, add: take }
}

#[cfg(test)]
mod tests {
    use super::*;
    use daw_core::time::{PPQ, SegmentTempoMap};

    fn on(time: i64, key: u8, velocity: f32) -> Captured {
        Captured::Event {
            time: SampleTime(time),
            kind: EventKind::NoteOn {
                channel: 0,
                key,
                velocity,
                note_id: -1,
            },
        }
    }

    fn off(time: i64, key: u8) -> Captured {
        Captured::Event {
            time: SampleTime(time),
            kind: EventKind::NoteOff {
                channel: 0,
                key: Some(key),
                velocity: 0.0,
                note_id: -1,
            },
        }
    }

    fn spans(notes: &[RecordedNote]) -> Vec<(i64, i64, u8)> {
        notes.iter().map(|n| (n.start.0, n.end.0, n.key)).collect()
    }

    #[test]
    fn pairs_on_and_off() {
        let mut r = Recorder::new();
        for c in [
            on(10, 60, 0.5),
            on(20, 64, 1.0),
            off(30, 60),
            off(35, 99),
            on(40, 64, 0.2),
        ] {
            r.feed(c);
        }
        assert_eq!(spans(r.notes()), [(10, 30, 60), (20, 40, 64)]);
        assert_eq!(r.held().count(), 1);
        let take = r.finish(Some(SampleTime(100)));
        assert_eq!(spans(&take), [(10, 30, 60), (20, 40, 64), (40, 100, 64)]);
        assert_eq!(take[2].velocity, 0.2);
        assert!(r.finish(None).is_empty());
    }

    #[test]
    fn wildcard_note_offs_end_every_key_or_one_note_id() {
        let ev = |time: i64, kind| Captured::Event {
            time: SampleTime(time),
            kind,
        };
        let on_id = |time, key, note_id| {
            ev(
                time,
                EventKind::NoteOn {
                    channel: 0,
                    key,
                    velocity: 1.0,
                    note_id,
                },
            )
        };
        let off_any = |time, channel, note_id| {
            ev(
                time,
                EventKind::NoteOff {
                    channel,
                    key: None,
                    velocity: 0.0,
                    note_id,
                },
            )
        };
        let mut r = Recorder::new();
        for c in [
            on_id(0, 60, 1),
            on_id(0, 64, 2),
            on_id(0, 67, -1),
            // Note 2 whatever its key.
            off_any(10, 0, 2),
            // Another channel holds nothing.
            off_any(15, 1, -1),
            // Every key.
            off_any(20, 0, -1),
        ] {
            r.feed(c);
        }
        assert_eq!(spans(r.notes()), [(0, 10, 64), (0, 20, 60), (0, 20, 67)]);
        assert_eq!(r.held().count(), 0);
    }

    #[test]
    fn raw_midi_and_zero_velocity() {
        let mut r = Recorder::new();
        let midi = |time, b: [u8; 3]| Captured::Event {
            time: SampleTime(time),
            kind: EventKind::Midi(b),
        };
        r.feed(midi(0, [0x91, 60, 127]));
        r.feed(midi(5, [0x91, 60, 0]));
        r.feed(midi(6, [0x92, 61, 64]));
        r.feed(midi(9, [0x82, 61, 0]));
        r.feed(midi(9, [0xb0, 1, 64]));
        r.feed(on(10, 62, 1.0));
        r.feed(on(12, 62, 0.0));
        let take = r.finish(None);
        assert_eq!(spans(&take), [(0, 5, 60), (6, 9, 61), (10, 12, 62)]);
        assert_eq!((take[0].channel, take[0].velocity), (1, 1.0));
        assert_eq!(take[1].channel, 2);
    }

    #[test]
    fn held_notes_carry_across_a_loop_wrap() {
        let mut r = Recorder::new();
        r.feed(on(900, 60, 0.5));
        r.feed(Captured::Jump {
            from: SampleTime(1000),
            to: SampleTime(0),
        });
        r.feed(off(50, 60));
        r.feed(on(60, 60, 0.5));
        r.feed(Captured::Stop { at: SampleTime(70) });
        let take = r.finish(None);
        assert_eq!(spans(&take), [(0, 50, 60), (60, 70, 60), (900, 1000, 60)]);
    }

    #[test]
    fn zero_length_notes_get_one_sample() {
        let mut r = Recorder::new();
        r.feed(on(10, 60, 0.5));
        r.feed(off(10, 60));
        assert_eq!(spans(&r.finish(None)), [(10, 11, 60)]);
    }

    #[test]
    fn converts_and_merges() {
        let tempo = SegmentTempoMap::new(120.0);
        let notes = [
            RecordedNote {
                start: SampleTime(24_000),
                end: SampleTime(36_000),
                key: 60,
                channel: 0,
                velocity: 0.5,
            },
            RecordedNote {
                start: SampleTime(0),
                end: SampleTime(30_000),
                key: 62,
                channel: 0,
                velocity: 0.5,
            },
        ];
        let data = to_clip_notes(
            &notes,
            &tempo,
            48_000.0,
            TickTime(PPQ / 2)..TickTime(4 * PPQ),
        );
        assert_eq!((data[0].start.0, data[0].length.0), (PPQ / 2, PPQ / 2));
        assert_eq!((data[1].start.0, data[1].length.0), (0, PPQ * 3 / 4));

        let existing = [
            NoteData::new(0, 10, 50, 1.0),
            NoteData::new(PPQ, 10, 50, 1.0),
        ];
        let plan = merge_take(
            &existing,
            data.clone(),
            TakeMode::Replace,
            TickTime(0)..TickTime(PPQ),
        );
        assert_eq!(plan.remove, [0]);
        assert_eq!(plan.add, data);
        let plan = merge_take(
            &existing,
            data,
            TakeMode::Overdub,
            TickTime(0)..TickTime(PPQ),
        );
        assert!(plan.remove.is_empty());
    }

    #[test]
    fn notes_outside_the_clip_are_dropped_or_cut() {
        let tempo = SegmentTempoMap::new(120.0);
        let rn = |s, e| RecordedNote {
            start: SampleTime(s),
            end: SampleTime(e),
            key: 60,
            channel: 0,
            velocity: 0.5,
        };
        // Clip from beat 1 to beat 2 (24,000..48,000 samples).
        let notes = [
            rn(0, 1000),
            rn(20_000, 30_000),
            rn(40_000, 60_000),
            rn(50_000, 60_000),
        ];
        let data = to_clip_notes(&notes, &tempo, 48_000.0, TickTime(PPQ)..TickTime(2 * PPQ));
        let spans: Vec<(i64, i64)> = data.iter().map(|n| (n.start.0, n.end().0)).collect();
        assert_eq!(spans, [(0, PPQ / 4), (PPQ * 2 / 3, PPQ)]);
    }
}
