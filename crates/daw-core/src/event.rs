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
    NoteOff {
        channel: u8,
        key: u8,
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

#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Event {
    /// Frame offset inside the current buffer.
    pub frame: u32,
    pub kind: EventKind,
}

/// A fixed-capacity list of events, kept sorted by frame.
///
/// Allocate it off the audio thread with [`EventList::with_capacity`]; after
/// that, [`push`](Self::push) and [`clear`](Self::clear) never allocate.
/// This is a straightforward implementation so other units can use it now;
/// WU1 owns and hardens it.
#[derive(Clone, Debug, Default)]
pub struct EventList {
    events: Vec<Event>,
}

impl EventList {
    pub fn with_capacity(n: usize) -> Self {
        Self {
            events: Vec::with_capacity(n),
        }
    }

    /// Inserts after any events at the same or an earlier frame, so events
    /// at equal frames keep their push order. Gives the event back when full.
    pub fn push(&mut self, e: Event) -> Result<(), Event> {
        if self.events.len() == self.events.capacity() {
            return Err(e);
        }
        let at = self.events.partition_point(|x| x.frame <= e.frame);
        self.events.insert(at, e);
        Ok(())
    }

    pub fn clear(&mut self) {
        self.events.clear();
    }

    /// Events in frame order.
    pub fn iter(&self) -> impl Iterator<Item = &Event> {
        self.events.iter()
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn midi(frame: u32, key: u8) -> Event {
        Event {
            frame,
            kind: EventKind::Midi([0x90, key, 100]),
        }
    }

    #[test]
    fn keeps_frame_order_and_push_order() {
        let mut list = EventList::with_capacity(8);
        for e in [midi(10, 1), midi(0, 2), midi(10, 3), midi(5, 4)] {
            list.push(e).unwrap();
        }
        let keys: Vec<u8> = list
            .iter()
            .map(|e| match e.kind {
                EventKind::Midi(data) => data[1],
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(keys, [2, 4, 1, 3]);
    }

    #[test]
    fn gives_the_event_back_when_full() {
        let mut list = EventList::with_capacity(2);
        let capacity = list.events.capacity();
        for i in 0..capacity {
            list.push(midi(i as u32, 0)).unwrap();
        }
        assert_eq!(list.push(midi(0, 9)), Err(midi(0, 9)));
        assert_eq!(list.events.capacity(), capacity);
    }
}
