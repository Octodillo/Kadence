//! The note value the editing code works on.

use daw_core::time::TickTime;

/// One note's editable values, without an identity. Times are in ticks
/// relative to the clip start.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct NoteData {
    pub start: TickTime,
    pub length: TickTime,
    pub key: u8,
    pub channel: u8,
    /// 0 to 1.
    pub velocity: f64,
}

impl NoteData {
    pub fn new(start: i64, length: i64, key: u8, velocity: f64) -> Self {
        Self {
            start: TickTime(start),
            length: TickTime(length),
            key,
            channel: 0,
            velocity,
        }
    }

    pub fn end(&self) -> TickTime {
        TickTime(self.start.0 + self.length.0)
    }

    /// Whether the note is in range: key and channel valid, start at or
    /// after 0, positive length, velocity within 0..1.
    pub fn is_valid(&self) -> bool {
        self.key <= 127
            && self.channel <= 15
            && self.start.0 >= 0
            && self.length.0 > 0
            && (0.0..=1.0).contains(&self.velocity)
    }
}

/// A note the editing operations can read and change in place. The model's
/// note type implements this, so edits work directly on copies of model
/// notes and keep their IDs.
pub trait EditNote {
    fn data(&self) -> NoteData;
    fn set_data(&mut self, data: NoteData);
}

impl EditNote for NoteData {
    fn data(&self) -> NoteData {
        *self
    }

    fn set_data(&mut self, data: NoteData) {
        *self = data;
    }
}
