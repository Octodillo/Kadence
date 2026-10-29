//! Clip notes compiled to sample time for the player.
//!
//! Compiling happens off the audio thread whenever notes, clips or the tempo
//! change. The [`MidiPlayer`](crate::player::MidiPlayer) then only compares
//! sample positions.

use daw_core::time::{SampleTime, TempoMap, TickTime};

use crate::edit::clamp_velocity;
use crate::note::NoteData;

/// One note on the timeline, in samples.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct SeqNote {
    pub start: SampleTime,
    /// Exclusive; always after `start`.
    pub end: SampleTime,
    pub key: u8,
    pub channel: u8,
    pub velocity: f32,
    /// Identifies the note across recompiles, so the player can tell an
    /// edited note from a new one while it sounds. Any value that is stable
    /// per model note works; see [`uid_from_u128`].
    pub uid: u64,
}

/// Folds a 128-bit note ID (the model's UUID) into a [`SeqNote::uid`].
pub fn uid_from_u128(id: u128) -> u64 {
    (id as u64) ^ ((id >> 64) as u64)
}

/// The notes one player plays, sorted by start.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sequence {
    notes: Vec<SeqNote>,
    // Longest note, so chasing only has to look this far back.
    max_len: i64,
}

impl Sequence {
    pub fn empty() -> Self {
        Self::default()
    }

    /// Sorts `notes` by start (then key, then uid) and drops notes that
    /// cannot sound: no length, an out-of-range key or channel, or a
    /// velocity that is not above 0 (a zero-velocity note-on means
    /// note-off to most instruments).
    pub fn new(mut notes: Vec<SeqNote>) -> Self {
        notes.retain(|n| {
            n.end > n.start
                && n.key <= 127
                && n.channel <= 15
                && n.velocity > 0.0
                && n.velocity <= 1.0
        });
        notes.sort_by_key(|n| (n.start, n.key, n.uid));
        let max_len = notes.iter().map(|n| n.end.0 - n.start.0).max().unwrap_or(0);
        Self { notes, max_len }
    }

    pub fn notes(&self) -> &[SeqNote] {
        &self.notes
    }

    pub fn len(&self) -> usize {
        self.notes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.notes.is_empty()
    }

    /// Length of the longest note, in samples.
    pub fn max_len(&self) -> i64 {
        self.max_len
    }

    /// Index of the first note starting at or after `pos`.
    pub fn first_at_or_after(&self, pos: i64) -> usize {
        self.notes.partition_point(|n| n.start.0 < pos)
    }

    /// Index range that holds every note sounding at `pos` (started before
    /// it, still playing). Notes in the range may have ended already; check
    /// `end > pos`.
    pub fn sounding_range(&self, pos: i64) -> std::ops::Range<usize> {
        let from = self.first_at_or_after(pos.saturating_sub(self.max_len));
        from..self.first_at_or_after(pos)
    }

    /// Notes that started before `pos` and are still sounding at it.
    pub fn sounding_at(&self, pos: i64) -> impl Iterator<Item = &SeqNote> {
        self.notes[self.sounding_range(pos)]
            .iter()
            .filter(move |n| n.end.0 > pos)
    }

    /// The note with this start and uid, if it is still there.
    pub fn find(&self, start: SampleTime, uid: u64) -> Option<&SeqNote> {
        let from = self.first_at_or_after(start.0);
        self.notes[from..]
            .iter()
            .take_while(|n| n.start == start)
            .find(|n| n.uid == uid)
    }
}

/// One clip's notes, for [`compile`].
#[derive(Clone, Debug)]
pub struct ClipNotes<I> {
    /// Where the clip starts on the timeline.
    pub start: TickTime,
    /// Notes are cut off at the clip's end.
    pub length: TickTime,
    /// `(uid, note)` pairs with clip-relative times.
    pub notes: I,
}

/// Compiles clips into one sequence, converting ticks to samples through
/// `tempo`. Notes starting at or after their clip's end are left out, and
/// notes running past it are cut short.
pub fn compile<I>(
    clips: impl IntoIterator<Item = ClipNotes<I>>,
    tempo: &dyn TempoMap,
    sample_rate: f64,
) -> Sequence
where
    I: IntoIterator<Item = (u64, NoteData)>,
{
    let mut out = Vec::new();
    for clip in clips {
        let clip_end = clip.start.0 + clip.length.0;
        for (uid, n) in clip.notes {
            if n.length.0 <= 0 || n.start.0 < 0 || n.start.0 >= clip.length.0 {
                continue;
            }
            let start = clip.start.0 + n.start.0;
            let end = (start + n.length.0).min(clip_end);
            out.push(SeqNote {
                start: tempo.ticks_to_samples(TickTime(start), sample_rate),
                end: tempo.ticks_to_samples(TickTime(end), sample_rate),
                key: n.key,
                channel: n.channel,
                velocity: clamp_velocity(n.velocity) as f32,
                uid,
            });
        }
    }
    Sequence::new(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use daw_core::time::{PPQ, SegmentTempoMap};

    #[test]
    fn compile_cuts_at_clip_end() {
        // 120 bpm at 48 kHz: one beat is 24,000 samples.
        let tempo = SegmentTempoMap::new(120.0);
        let clips = [
            ClipNotes {
                start: TickTime(PPQ),
                length: TickTime(2 * PPQ),
                notes: vec![
                    (1, NoteData::new(0, PPQ, 60, 0.5)),
                    (2, NoteData::new(PPQ, 4 * PPQ, 62, 0.5)),
                    (3, NoteData::new(2 * PPQ, PPQ, 64, 0.5)),
                ],
            },
            ClipNotes {
                start: TickTime(0),
                length: TickTime(PPQ),
                notes: vec![(4, NoteData::new(0, PPQ / 2, 48, 1.0))],
            },
        ];
        let seq = compile(clips, &tempo, 48_000.0);
        let got: Vec<(i64, i64, u64)> = seq
            .notes()
            .iter()
            .map(|n| (n.start.0, n.end.0, n.uid))
            .collect();
        assert_eq!(
            got,
            [(0, 12_000, 4), (24_000, 48_000, 1), (48_000, 72_000, 2)]
        );
    }

    #[test]
    fn lookups() {
        let n = |s, e, uid| SeqNote {
            start: SampleTime(s),
            end: SampleTime(e),
            key: 60,
            channel: 0,
            velocity: 1.0,
            uid,
        };
        let seq = Sequence::new(vec![
            n(100, 1000, 1),
            n(0, 50, 2),
            n(500, 600, 3),
            n(5, 5, 9),
        ]);
        assert_eq!(seq.len(), 3);
        assert_eq!(seq.first_at_or_after(100), 1);
        let sounding: Vec<u64> = seq.sounding_at(550).map(|n| n.uid).collect();
        assert_eq!(sounding, [1, 3]);
        assert_eq!(seq.sounding_at(500).count(), 1);
        assert_eq!(seq.find(SampleTime(500), 3).map(|n| n.end.0), Some(600));
        assert!(seq.find(SampleTime(500), 1).is_none());
        assert_eq!(uid_from_u128(1 | (3 << 64)), 2);
    }

    #[test]
    fn silent_notes_are_not_played() {
        let tempo = SegmentTempoMap::new(120.0);
        let seq = compile(
            [ClipNotes {
                start: TickTime(0),
                length: TickTime(PPQ),
                notes: vec![
                    (1, NoteData::new(0, 10, 60, 0.0)),
                    (2, NoteData::new(0, 10, 61, f64::NAN)),
                    (3, NoteData::new(0, 10, 62, 0.5)),
                ],
            }],
            &tempo,
            48_000.0,
        );
        assert_eq!(seq.len(), 1);
        assert_eq!(seq.notes()[0].uid, 3);
    }
}
