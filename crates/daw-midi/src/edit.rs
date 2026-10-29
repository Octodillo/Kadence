//! Piano-roll editing operations.
//!
//! Each operation works on a slice of notes (the selection) through
//! [`EditNote`], so it can run on copies of the model's notes and keep their
//! IDs. The caller turns the edited copies into one undoable command.
//! Operations that would push a note out of range clamp the whole group, so
//! chords and phrases keep their shape.

use daw_core::time::TickTime;

use crate::grid::{Grid, Meter};
use crate::note::{EditNote, NoteData};

/// Shortest note an edit produces, in ticks.
pub const MIN_LENGTH: i64 = 1;

/// A grid seen from inside a clip: note times are clip-relative, grid lines
/// are on the timeline, and `origin` is where the clip starts.
#[derive(Copy, Clone, Debug)]
pub struct Snap<'a> {
    pub grid: &'a Grid,
    pub meter: &'a Meter,
    pub origin: TickTime,
}

impl<'a> Snap<'a> {
    pub fn new(grid: &'a Grid, meter: &'a Meter, origin: TickTime) -> Self {
        Self {
            grid,
            meter,
            origin,
        }
    }

    fn map(&self, t: TickTime, f: impl Fn(&Grid, TickTime, &Meter) -> TickTime) -> TickTime {
        TickTime(f(self.grid, TickTime(t.0 + self.origin.0), self.meter).0 - self.origin.0)
    }

    pub fn snap(&self, t: TickTime) -> TickTime {
        self.map(t, Grid::snap)
    }

    pub fn floor(&self, t: TickTime) -> TickTime {
        self.map(t, Grid::floor)
    }

    pub fn ceil(&self, t: TickTime) -> TickTime {
        self.map(t, Grid::ceil)
    }

    pub fn next(&self, t: TickTime) -> TickTime {
        self.map(t, Grid::next)
    }

    pub fn step_at(&self, t: TickTime) -> i64 {
        self.grid.step_at(TickTime(t.0 + self.origin.0), self.meter)
    }
}

fn update<N: EditNote>(notes: &mut [N], mut f: impl FnMut(&mut NoteData)) {
    for n in notes {
        let mut d = n.data();
        f(&mut d);
        n.set_data(d);
    }
}

/// The earliest start and latest end of `notes`.
pub fn span<N: EditNote>(notes: &[N]) -> Option<(TickTime, TickTime)> {
    let start = notes.iter().map(|n| n.data().start).min()?;
    let end = notes.iter().map(|n| n.data().end()).max()?;
    Some((start, end))
}

// ---------------------------------------------------------------------------
// Quantize

/// What quantize does to note ends.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum QuantizeEnds {
    /// Keep each note's length, so the end moves with the start.
    #[default]
    KeepLength,
    /// Snap ends to the grid too. A note shorter than a step keeps at least
    /// one step.
    Snap,
    /// Leave ends where they are, so the length absorbs the start's move.
    Keep,
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Quantize {
    /// 0 to 1: how far each note moves towards its grid line.
    pub strength: f64,
    pub ends: QuantizeEnds,
}

impl Default for Quantize {
    fn default() -> Self {
        Self {
            strength: 1.0,
            ends: QuantizeEnds::KeepLength,
        }
    }
}

fn towards(from: i64, to: i64, strength: f64) -> i64 {
    from + ((to - from) as f64 * strength).round() as i64
}

/// Moves note starts (and, by `q.ends`, their ends) towards the grid.
pub fn quantize<N: EditNote>(notes: &mut [N], snap: &Snap, q: Quantize) {
    let strength = if q.strength.is_finite() {
        q.strength.clamp(0.0, 1.0)
    } else {
        0.0
    };
    update(notes, |d| {
        let end = d.end().0;
        let start = towards(d.start.0, snap.snap(d.start).0, strength).max(0);
        let new_end = match q.ends {
            QuantizeEnds::KeepLength => start + d.length.0,
            QuantizeEnds::Keep => end,
            QuantizeEnds::Snap => {
                let target = snap.snap(d.end()).0;
                let target = if target <= start {
                    snap.next(TickTime(start)).0
                } else {
                    target
                };
                towards(end, target, strength)
            }
        };
        d.start = TickTime(start);
        d.length = TickTime((new_end - start).max(MIN_LENGTH));
    });
}

// ---------------------------------------------------------------------------
// Moving and resizing

/// Moves notes by `dt` ticks and `dkey` semitones. Both are clamped so that
/// no note starts before 0 or leaves the key range; returns what was applied.
pub fn shift<N: EditNote>(notes: &mut [N], dt: i64, dkey: i32) -> (i64, i32) {
    let Some(min_start) = notes.iter().map(|n| n.data().start.0).min() else {
        return (0, 0);
    };
    let lo = notes.iter().map(|n| n.data().key as i32).min().unwrap_or(0);
    let hi = notes
        .iter()
        .map(|n| n.data().key as i32)
        .max()
        .unwrap_or(127);
    let dt = dt.max(-min_start);
    let dkey = dkey.clamp(-lo, 127 - hi);
    update(notes, |d| {
        d.start = TickTime(d.start.0 + dt);
        d.key = (d.key as i32 + dkey) as u8;
    });
    (dt, dkey)
}

/// Transposes by `semitones`, clamped to keep every note in range.
pub fn transpose<N: EditNote>(notes: &mut [N], semitones: i32) -> i32 {
    shift(notes, 0, semitones).1
}

/// Moves notes in time by `dt`, clamped at 0.
pub fn nudge<N: EditNote>(notes: &mut [N], dt: i64) -> i64 {
    shift(notes, dt, 0).0
}

/// Moves every note's end by `delta`, keeping at least [`MIN_LENGTH`].
pub fn resize_end<N: EditNote>(notes: &mut [N], delta: i64) {
    update(notes, |d| {
        d.length = TickTime((d.length.0 + delta).max(MIN_LENGTH))
    });
}

/// Moves every note's start by `delta` while its end stays put, keeping at
/// least [`MIN_LENGTH`] and a start at or after 0.
pub fn resize_start<N: EditNote>(notes: &mut [N], delta: i64) {
    update(notes, |d| {
        let end = d.end().0;
        let start = (d.start.0 + delta).clamp(0, end - MIN_LENGTH);
        d.start = TickTime(start);
        d.length = TickTime(end - start);
    });
}

/// Gives every note the same length.
pub fn set_length<N: EditNote>(notes: &mut [N], length: i64) {
    let length = length.max(MIN_LENGTH);
    update(notes, |d| d.length = TickTime(length));
}

/// Stretches starts and lengths around `anchor` by `factor` (2 is half
/// speed, 0.5 double speed).
pub fn scale_time<N: EditNote>(notes: &mut [N], anchor: TickTime, factor: f64) {
    if !factor.is_finite() || factor <= 0.0 {
        return;
    }
    update(notes, |d| {
        let start = anchor.0 as f64 + (d.start.0 - anchor.0) as f64 * factor;
        let end = anchor.0 as f64 + (d.end().0 - anchor.0) as f64 * factor;
        let start = (start.round() as i64).max(0);
        d.start = TickTime(start);
        d.length = TickTime((end.round() as i64 - start).max(MIN_LENGTH));
    });
}

/// Extends each note to the start of the next later note in the selection.
/// The notes of the last chord keep their length.
pub fn legato<N: EditNote>(notes: &mut [N]) {
    let mut starts: Vec<i64> = notes.iter().map(|n| n.data().start.0).collect();
    starts.sort_unstable();
    starts.dedup();
    update(notes, |d| {
        let i = starts.partition_point(|&s| s <= d.start.0);
        if let Some(&next) = starts.get(i) {
            d.length = TickTime(next - d.start.0);
        }
    });
}

/// Cuts each note short where the next note on the same key and channel
/// starts. Returns the indices of notes left with nothing to play (a later
/// note starts at the same tick on the same key); delete those.
pub fn trim_overlaps<N: EditNote>(notes: &mut [N]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..notes.len()).collect();
    order.sort_by_key(|&i| {
        let d = notes[i].data();
        (d.channel, d.key, d.start, i)
    });
    let mut dead = Vec::new();
    for w in order.windows(2) {
        let (a, b) = (notes[w[0]].data(), notes[w[1]].data());
        if a.channel != b.channel || a.key != b.key || a.end() <= b.start {
            continue;
        }
        if b.start <= a.start {
            dead.push(w[0]);
        } else {
            let mut a = a;
            a.length = TickTime(b.start.0 - a.start.0);
            notes[w[0]].set_data(a);
        }
    }
    dead.sort_unstable();
    dead
}

/// Splits a note at `at` into a left and right part, or `None` when `at`
/// is not strictly inside it.
pub fn split(note: NoteData, at: TickTime) -> Option<(NoteData, NoteData)> {
    if at <= note.start || at >= note.end() {
        return None;
    }
    let mut left = note;
    left.length = TickTime(at.0 - note.start.0);
    let mut right = note;
    right.start = at;
    right.length = TickTime(note.end().0 - at.0);
    Some((left, right))
}

/// Which notes [`glue`] keeps (with their new values) and which it removes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GluePlan {
    pub keep: Vec<(usize, NoteData)>,
    pub remove: Vec<usize>,
}

/// Joins notes on the same key and channel that touch, overlap or are at
/// most `gap` ticks apart into one note each. The first note of each run
/// is kept, with its velocity.
pub fn glue<N: EditNote>(notes: &[N], gap: i64) -> GluePlan {
    let mut order: Vec<usize> = (0..notes.len()).collect();
    order.sort_by_key(|&i| {
        let d = notes[i].data();
        (d.channel, d.key, d.start, i)
    });
    let mut plan = GluePlan::default();
    let mut run: Option<(usize, NoteData, bool)> = None;
    for i in order {
        let d = notes[i].data();
        match &mut run {
            Some((_, head, grew))
                if head.channel == d.channel
                    && head.key == d.key
                    && d.start.0 <= head.end().0 + gap.max(0) =>
            {
                let end = head.end().max(d.end());
                head.length = TickTime(end.0 - head.start.0);
                *grew = true;
                plan.remove.push(i);
            }
            _ => {
                if let Some((k, head, true)) = run.take() {
                    plan.keep.push((k, head));
                }
                run = Some((i, d, false));
            }
        }
    }
    if let Some((k, head, true)) = run {
        plan.keep.push((k, head));
    }
    plan.remove.sort_unstable();
    plan
}

/// Copies of `notes` moved `offset` ticks later.
pub fn duplicate<N: EditNote>(notes: &[N], offset: i64) -> Vec<NoteData> {
    notes
        .iter()
        .map(|n| {
            let mut d = n.data();
            d.start = TickTime((d.start.0 + offset).max(0));
            d
        })
        .collect()
}

/// How far "Duplicate" moves a copy: the selection's span rounded up to
/// the grid, so the copy lands right after the original on a grid line.
pub fn duplicate_offset<N: EditNote>(notes: &[N], snap: Option<&Snap>) -> i64 {
    let Some((start, end)) = span(notes) else {
        return 0;
    };
    match snap {
        Some(s) => {
            let from = s.floor(start);
            (s.ceil(end).0 - from.0).max(s.step_at(start))
        }
        None => end.0 - start.0,
    }
}

/// Mirrors notes in time inside their span (retrograde).
pub fn reverse<N: EditNote>(notes: &mut [N]) {
    let Some((start, end)) = span(notes) else {
        return;
    };
    update(notes, |d| d.start = TickTime(start.0 + end.0 - d.end().0));
}

/// Mirrors keys around `axis` (inversion), clamped to the key range.
pub fn invert<N: EditNote>(notes: &mut [N], axis: u8) {
    update(notes, |d| {
        d.key = (2 * axis as i32 - d.key as i32).clamp(0, 127) as u8;
    });
}

// ---------------------------------------------------------------------------
// Velocity

fn clamp_velocity(v: f64) -> f64 {
    if v.is_finite() {
        v.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

pub fn set_velocity<N: EditNote>(notes: &mut [N], velocity: f64) {
    let v = clamp_velocity(velocity);
    update(notes, |d| d.velocity = v);
}

pub fn scale_velocity<N: EditNote>(notes: &mut [N], factor: f64) {
    update(notes, |d| d.velocity = clamp_velocity(d.velocity * factor));
}

pub fn add_velocity<N: EditNote>(notes: &mut [N], delta: f64) {
    update(notes, |d| d.velocity = clamp_velocity(d.velocity + delta));
}

/// Sets velocities along a straight line from `from` at the first start to
/// `to` at the last start (a crescendo or a fade).
pub fn ramp_velocity<N: EditNote>(notes: &mut [N], from: f64, to: f64) {
    let Some(first) = notes.iter().map(|n| n.data().start.0).min() else {
        return;
    };
    let last = notes
        .iter()
        .map(|n| n.data().start.0)
        .max()
        .unwrap_or(first);
    let span = (last - first) as f64;
    update(notes, |d| {
        let x = if span > 0.0 {
            (d.start.0 - first) as f64 / span
        } else {
            0.0
        };
        d.velocity = clamp_velocity(from + (to - from) * x);
    });
}

// ---------------------------------------------------------------------------
// Humanize

/// SplitMix64: a tiny deterministic generator, so humanize is repeatable
/// from its seed and redo gives the same result.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in -1..1.
    fn signed(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 52) as f64 - 1.0
    }
}

/// Adds random offsets of up to `timing` ticks to starts and up to
/// `velocity` to velocities. The same seed gives the same result.
pub fn humanize<N: EditNote>(notes: &mut [N], timing: i64, velocity: f64, seed: u64) {
    let mut rng = Rng(seed);
    let timing = timing.max(0) as f64;
    let velocity = clamp_velocity(velocity);
    update(notes, |d| {
        let dt = (rng.signed() * timing).round() as i64;
        let dv = rng.signed() * velocity;
        d.start = TickTime((d.start.0 + dt).max(0));
        d.velocity = clamp_velocity(d.velocity + dv);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use daw_core::time::PPQ;

    const Q: i64 = PPQ;
    const S: i64 = PPQ / 4;

    fn n(start: i64, length: i64, key: u8) -> NoteData {
        NoteData::new(start, length, key, 0.8)
    }

    fn starts(notes: &[NoteData]) -> Vec<i64> {
        notes.iter().map(|n| n.start.0).collect()
    }

    #[test]
    fn quantize_full_and_half_strength() {
        let grid = Grid::note(16);
        let meter = Meter::default();
        let snap = Snap::new(&grid, &meter, TickTime(0));
        let mut notes = [n(10, 100, 60), n(S - 30, 100, 62), n(2 * S + 120, 50, 64)];
        quantize(&mut notes, &snap, Quantize::default());
        assert_eq!(starts(&notes), [0, S, 3 * S]);
        assert!(notes.iter().all(|x| x.length.0 == 100 || x.length.0 == 50));

        let mut notes = [n(100, 100, 60)];
        quantize(
            &mut notes,
            &snap,
            Quantize {
                strength: 0.5,
                ..Quantize::default()
            },
        );
        assert_eq!(notes[0].start.0, 50);
    }

    #[test]
    fn quantize_respects_clip_origin() {
        // The clip starts 100 ticks after a grid line, so the grid lines
        // fall at -100, 140, 380... in clip time.
        let grid = Grid::note(16);
        let meter = Meter::default();
        let snap = Snap::new(&grid, &meter, TickTime(100));
        let mut notes = [n(130, 50, 60), n(10, 50, 60)];
        quantize(&mut notes, &snap, Quantize::default());
        assert_eq!(starts(&notes), [140, 0]);
    }

    #[test]
    fn quantize_ends() {
        let grid = Grid::note(16);
        let meter = Meter::default();
        let snap = Snap::new(&grid, &meter, TickTime(0));
        let snap_ends = Quantize {
            ends: QuantizeEnds::Snap,
            ..Quantize::default()
        };
        let mut notes = [n(10, S + 20, 60), n(S, 5, 62)];
        quantize(&mut notes, &snap, snap_ends);
        assert_eq!((notes[0].start.0, notes[0].length.0), (0, S));
        // Too short to reach a line: it keeps one step.
        assert_eq!((notes[1].start.0, notes[1].length.0), (S, S));

        let mut notes = [n(10, 100, 60)];
        quantize(
            &mut notes,
            &snap,
            Quantize {
                ends: QuantizeEnds::Keep,
                ..Quantize::default()
            },
        );
        assert_eq!((notes[0].start.0, notes[0].end().0), (0, 110));
    }

    #[test]
    fn shift_clamps_the_group() {
        let mut notes = [n(100, 10, 120), n(500, 10, 60)];
        assert_eq!(shift(&mut notes, -300, 20), (-100, 7));
        assert_eq!(starts(&notes), [0, 400]);
        assert_eq!((notes[0].key, notes[1].key), (127, 67));
        assert_eq!(transpose(&mut notes, -200), -67);
        assert_eq!(notes[1].key, 0);
        assert_eq!(nudge(&mut [] as &mut [NoteData], 5), 0);
    }

    #[test]
    fn resizing_keeps_a_minimum() {
        let mut notes = [n(100, 50, 60)];
        resize_end(&mut notes, -1000);
        assert_eq!(notes[0].length.0, MIN_LENGTH);
        let mut notes = [n(100, 50, 60)];
        resize_start(&mut notes, 30);
        assert_eq!((notes[0].start.0, notes[0].end().0), (130, 150));
        resize_start(&mut notes, -1000);
        assert_eq!((notes[0].start.0, notes[0].end().0), (0, 150));
        resize_start(&mut notes, 1000);
        assert_eq!((notes[0].start.0, notes[0].length.0), (149, 1));
        set_length(&mut notes, 0);
        assert_eq!(notes[0].length.0, MIN_LENGTH);
    }

    #[test]
    fn scale_time_around_anchor() {
        let mut notes = [n(Q, Q, 60), n(2 * Q, Q / 2, 62)];
        scale_time(&mut notes, TickTime(Q), 2.0);
        assert_eq!(starts(&notes), [Q, 3 * Q]);
        assert_eq!((notes[0].length.0, notes[1].length.0), (2 * Q, Q));
    }

    #[test]
    fn legato_reaches_next_start() {
        // A two-note chord, then a single note.
        let mut notes = [n(0, 10, 60), n(0, 10, 64), n(Q, 10, 67)];
        legato(&mut notes);
        assert_eq!(
            notes.iter().map(|x| x.length.0).collect::<Vec<_>>(),
            [Q, Q, 10]
        );
    }

    #[test]
    fn trim_overlaps_on_same_key() {
        let mut notes = [
            n(0, 500, 60),
            n(300, 500, 60),
            n(300, 10, 62),
            n(300, 10, 60),
        ];
        let dead = trim_overlaps(&mut notes);
        assert_eq!(notes[0].length.0, 300);
        // Notes 1 and 3 start together on key 60: the earlier index goes.
        assert_eq!(dead, [1]);
        assert_eq!(notes[2].length.0, 10);
    }

    #[test]
    fn split_and_glue() {
        let note = n(100, 200, 60);
        let (l, r) = split(note, TickTime(150)).unwrap();
        assert_eq!(
            (l.start.0, l.length.0, r.start.0, r.length.0),
            (100, 50, 150, 150)
        );
        assert!(split(note, TickTime(100)).is_none());
        assert!(split(note, TickTime(300)).is_none());

        let notes = [l, r, n(400, 10, 60), n(100, 50, 61)];
        let plan = glue(&notes, 0);
        assert_eq!(plan.keep, [(0, note)]);
        assert_eq!(plan.remove, [1]);
        let plan = glue(&notes, 100);
        assert_eq!(plan.keep[0].1.end().0, 410);
        assert_eq!(plan.remove, [1, 2]);
    }

    #[test]
    fn duplicate_lands_after_on_grid() {
        let grid = Grid::note(4);
        let meter = Meter::default();
        let snap = Snap::new(&grid, &meter, TickTime(0));
        let notes = [n(10, Q, 60), n(Q, 100, 62)];
        assert_eq!(duplicate_offset(&notes, Some(&snap)), 2 * Q);
        assert_eq!(duplicate_offset(&notes, None), Q + 90);
        let copies = duplicate(&notes, 2 * Q);
        assert_eq!(starts(&copies), [2 * Q + 10, 3 * Q]);
    }

    #[test]
    fn reverse_and_invert() {
        let mut notes = [n(0, 100, 60), n(300, 100, 64)];
        reverse(&mut notes);
        assert_eq!(starts(&notes), [300, 0]);
        invert(&mut notes, 62);
        assert_eq!((notes[0].key, notes[1].key), (64, 60));
    }

    #[test]
    fn velocity_edits_clamp() {
        let mut notes = [n(0, 10, 60), n(100, 10, 60), n(200, 10, 60)];
        scale_velocity(&mut notes, 2.0);
        assert!(notes.iter().all(|x| x.velocity == 1.0));
        add_velocity(&mut notes, -0.25);
        assert!(notes.iter().all(|x| x.velocity == 0.75));
        ramp_velocity(&mut notes, 0.2, 1.0);
        let v: Vec<f64> = notes.iter().map(|x| x.velocity).collect();
        assert!((v[0] - 0.2).abs() < 1e-9 && (v[1] - 0.6).abs() < 1e-9 && v[2] == 1.0);
        set_velocity(&mut notes, f64::NAN);
        assert!(notes.iter().all(|x| x.velocity == 0.0));
    }

    #[test]
    fn humanize_is_seeded_and_bounded() {
        let base: Vec<NoteData> = (0..50).map(|i| n(1000 + i * 100, 50, 60)).collect();
        let mut a = base.clone();
        let mut b = base.clone();
        humanize(&mut a, 20, 0.1, 7);
        humanize(&mut b, 20, 0.1, 7);
        assert_eq!(a, b);
        assert_ne!(a, base);
        for (x, y) in a.iter().zip(&base) {
            assert!((x.start.0 - y.start.0).abs() <= 20);
            assert!((x.velocity - y.velocity).abs() <= 0.1 + 1e-12);
        }
    }
}
