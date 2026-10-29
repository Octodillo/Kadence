//! Stable identifiers for model objects.
//!
//! IDs are random UUIDs, so they stay valid across saves, undo and copy/paste
//! between projects. Each object kind has its own type so they can't be mixed
//! up.

use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! id_type {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub Uuid);

        impl $name {
            /// A fresh random ID.
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, concat!(stringify!($name), "({})"), self.0)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, f)
            }
        }
    };
}

id_type!(
    /// Identifies a track, including the master track.
    TrackId
);
id_type!(
    /// Identifies a clip on any track.
    ClipId
);
id_type!(
    /// Identifies a device in a track's device chain.
    DeviceId
);
id_type!(
    /// Identifies a send from one track to a bus.
    SendId
);
id_type!(
    /// Identifies an automation lane.
    LaneId
);
id_type!(
    /// Identifies a note inside a MIDI clip. Not the same as
    /// `daw_core::event::NoteId`, which is the per-voice ID in event streams.
    NoteUid
);
