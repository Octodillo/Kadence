//! Timestamped events: notes, raw MIDI and parameter changes.

/// Identifies one note across on, off and expression events. `-1` = none.
pub type NoteId = i32;

/// Identifies a parameter of a node or plugin (a CLAP `clap_id` for CLAP).
pub type ParamId = u32;

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum EventKind {
    NoteOn {
        channel: u8,
        key: u8,
        velocity: f32,
        note_id: NoteId,
    },
    /// Releases notes on `channel`. `key: None` is a wildcard: every key, or with a `note_id` of
    /// 0 or more, that note whatever its key. Targets with no wildcard form (MIDI, LV2, VST3)
    /// get MIDI all notes off (CC 123) on the channel for a wildcard.
    NoteOff {
        channel: u8,
        key: Option<u8>,
        velocity: f32,
        note_id: NoteId,
    },
    Midi([u8; 3]),
    /// Plain (not normalised) value.
    ParamValue {
        param: ParamId,
        value: f64,
    },
}

/// MIDI all notes off (CC 123) on `channel`: the MIDI form of a wildcard
/// [`EventKind::NoteOff`].
pub const fn all_notes_off(channel: u8) -> [u8; 3] {
    [0xb0 | (channel & 0x0f), 123, 0]
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Event {
    /// Frame offset inside the current buffer.
    pub frame: u32,
    pub kind: EventKind,
}

/// A fixed-capacity list of events kept sorted by frame.
///
/// All storage is allocated by [`EventList::with_capacity`]; `push`, `clear` and
/// iteration never allocate, so the list is safe to use from `Node::process`.
/// Events with equal frames keep their insertion order.
#[derive(Debug, Default)]
pub struct EventList {
    events: Vec<Event>,
    // `Vec::with_capacity` may round up, so the logical limit is tracked separately.
    capacity: usize,
    /// Pushes refused since [`take_overflow`](Self::take_overflow).
    overflow: u64,
}

impl EventList {
    pub fn with_capacity(n: usize) -> Self {
        Self {
            events: Vec::with_capacity(n),
            capacity: n,
            overflow: 0,
        }
    }

    /// Inserts `e` after every event whose frame is `<= e.frame`.
    ///
    /// Returns the event back when the list is full, and counts it for
    /// [`take_overflow`](Self::take_overflow), so a caller with nothing better
    /// to do than drop the event can ignore the result. Never allocates.
    pub fn push(&mut self, e: Event) -> Result<(), Event> {
        if self.events.len() >= self.capacity {
            self.overflow += 1;
            return Err(e);
        }
        // Events almost always arrive in order, so check the tail before searching.
        match self.events.last() {
            Some(last) if last.frame > e.frame => {
                let at = self.events.partition_point(|x| x.frame <= e.frame);
                self.events.insert(at, e);
            }
            _ => self.events.push(e),
        }
        Ok(())
    }

    /// How many events [`push`](Self::push) refused since the last call:
    /// what whoever filled the list had to drop. The audio thread cannot
    /// report a full list, so the owner of the list reads this instead.
    /// [`clear`](Self::clear) leaves the count alone.
    pub fn take_overflow(&mut self) -> u64 {
        std::mem::take(&mut self.overflow)
    }

    pub fn clear(&mut self) {
        self.events.clear();
    }

    /// Keeps the events `keep` returns true for, in order. Never allocates.
    pub fn retain(&mut self, mut keep: impl FnMut(&Event) -> bool) {
        self.events.retain(|e| keep(e));
    }

    /// Iterates the events sorted by frame.
    pub fn iter(&self) -> impl Iterator<Item = &Event> {
        self.events.iter()
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn is_full(&self) -> bool {
        self.events.len() >= self.capacity
    }

    /// The events sorted by frame.
    pub fn as_slice(&self) -> &[Event] {
        &self.events
    }
}

// Derived `Clone` would size the copy to its length, so the first `push` on
// a clone could allocate.
impl Clone for EventList {
    fn clone(&self) -> Self {
        let mut events = Vec::with_capacity(self.capacity);
        events.extend_from_slice(&self.events);
        Self {
            events,
            capacity: self.capacity,
            overflow: self.overflow,
        }
    }
}

impl<'a> IntoIterator for &'a EventList {
    type Item = &'a Event;
    type IntoIter = std::slice::Iter<'a, Event>;

    fn into_iter(self) -> Self::IntoIter {
        self.events.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(frame: u32, tag: u8) -> Event {
        Event {
            frame,
            kind: EventKind::Midi([tag, 0, 0]),
        }
    }

    fn tags(list: &EventList) -> Vec<(u32, u8)> {
        list.iter()
            .map(|e| match e.kind {
                EventKind::Midi([t, ..]) => (e.frame, t),
                _ => unreachable!(),
            })
            .collect()
    }

    #[test]
    fn keeps_events_sorted_and_stable() {
        let mut list = EventList::with_capacity(8);
        for (f, t) in [(5, 0), (1, 1), (5, 2), (0, 3), (1, 4), (9, 5)] {
            list.push(ev(f, t)).unwrap();
        }
        assert_eq!(
            tags(&list),
            [(0, 3), (1, 1), (1, 4), (5, 0), (5, 2), (9, 5)]
        );
    }

    #[test]
    fn rejects_when_full() {
        let mut list = EventList::with_capacity(2);
        list.push(ev(0, 0)).unwrap();
        list.push(ev(1, 1)).unwrap();
        assert!(list.is_full());
        assert_eq!(list.push(ev(2, 2)), Err(ev(2, 2)));
        assert_eq!(list.len(), 2);
        list.clear();
        assert!(list.is_empty());
        list.push(ev(2, 2)).unwrap();
    }

    #[test]
    fn refused_pushes_are_counted_until_taken() {
        let mut list = EventList::with_capacity(3);
        for e in [ev(4, 0), ev(2, 1), ev(6, 2), ev(0, 3), ev(1, 4)] {
            let _ = list.push(e);
        }
        assert_eq!(tags(&list), [(2, 1), (4, 0), (6, 2)]);
        list.clear();
        assert_eq!(list.take_overflow(), 2, "clearing does not forget them");
        assert_eq!(list.take_overflow(), 0);
        list.push(ev(0, 0)).unwrap();
        assert_eq!(list.take_overflow(), 0);
    }

    #[test]
    fn retain_keeps_order_and_capacity() {
        let mut list = EventList::with_capacity(3);
        for (f, t) in [(0, 0), (1, 1), (2, 2)] {
            list.push(ev(f, t)).unwrap();
        }
        list.retain(|e| e.frame != 1);
        assert_eq!(tags(&list), [(0, 0), (2, 2)]);
        list.push(ev(3, 3)).unwrap();
        assert!(list.is_full());
    }

    #[test]
    fn zero_capacity_rejects_everything() {
        let mut list = EventList::with_capacity(0);
        assert!(list.push(ev(0, 0)).is_err());
    }

    proptest::proptest! {
        #[test]
        fn matches_stable_sort(frames in proptest::collection::vec(0u32..64, 0..200)) {
            let mut list = EventList::with_capacity(frames.len());
            let mut expected = Vec::new();
            for (i, &f) in frames.iter().enumerate() {
                list.push(ev(f, i as u8)).unwrap();
                expected.push((f, i as u8));
            }
            expected.sort_by_key(|&(f, _)| f);
            proptest::prop_assert_eq!(tags(&list), expected);
        }
    }
}
