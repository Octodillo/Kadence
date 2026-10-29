//! Conversions between engine events and raw MIDI bytes. RT-safe.

use daw_core::event::EventKind;

/// Encodes a note or MIDI event. Returns the bytes and how many are used, or
/// `None` for events that are not MIDI (parameter changes).
pub(crate) fn to_midi(kind: &EventKind) -> Option<([u8; 3], usize)> {
    match *kind {
        EventKind::NoteOn {
            channel,
            key,
            velocity,
            ..
        } => {
            // Velocity 0 would mean note-off, so a quiet note-on is clamped to 1.
            let v = (velocity * 127.0).round();
            // NaN would cast to 0, which would turn the note into a note-off.
            let v = if v.is_nan() {
                1
            } else {
                v.clamp(1.0, 127.0) as u8
            };
            Some(([0x90 | (channel & 0x0f), key & 0x7f, v], 3))
        }
        EventKind::NoteOff {
            channel,
            key,
            velocity,
            ..
        } => {
            let v = (velocity * 127.0).round().clamp(0.0, 127.0) as u8;
            Some(([0x80 | (channel & 0x0f), key & 0x7f, v], 3))
        }
        EventKind::Midi(bytes) => Some((bytes, midi_len(bytes[0])?)),
        EventKind::ParamValue { .. } => None,
    }
}

/// Number of bytes in a short MIDI message with this status byte. `None` for
/// data bytes and SysEx, which do not fit in an [`EventKind::Midi`].
pub(crate) fn midi_len(status: u8) -> Option<usize> {
    match status {
        0x80..=0xbf | 0xe0..=0xef => Some(3),
        0xc0..=0xdf => Some(2),
        0xf1 | 0xf3 => Some(2),
        0xf2 => Some(3),
        0xf6 | 0xf8..=0xff => Some(1),
        _ => None,
    }
}

/// Decodes MIDI from a plugin's output. Note on/off become note events; other
/// short messages pass through as [`EventKind::Midi`]; SysEx is dropped.
pub(crate) fn from_midi(data: &[u8]) -> Option<EventKind> {
    let status = *data.first()?;
    let len = midi_len(status)?;
    if data.len() < len || data[1..len].iter().any(|&b| b >= 0x80) {
        return None;
    }
    let channel = status & 0x0f;
    match status & 0xf0 {
        0x90 if data[2] > 0 => Some(EventKind::NoteOn {
            channel,
            key: data[1],
            velocity: f32::from(data[2]) / 127.0,
            note_id: -1,
        }),
        0x80 | 0x90 => Some(EventKind::NoteOff {
            channel,
            key: data[1],
            velocity: if status & 0xf0 == 0x80 {
                f32::from(data[2]) / 127.0
            } else {
                0.0
            },
            note_id: -1,
        }),
        _ => {
            let mut bytes = [0; 3];
            bytes[..len].copy_from_slice(&data[..len]);
            Some(EventKind::Midi(bytes))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_round_trip() {
        let on = EventKind::NoteOn {
            channel: 3,
            key: 60,
            velocity: 100.0 / 127.0,
            note_id: -1,
        };
        let (bytes, len) = to_midi(&on).unwrap();
        assert_eq!(&bytes[..len], &[0x93, 60, 100]);
        assert_eq!(from_midi(&bytes[..len]), Some(on));

        let off = EventKind::NoteOff {
            channel: 0,
            key: 61,
            velocity: 0.0,
            note_id: -1,
        };
        let (bytes, len) = to_midi(&off).unwrap();
        assert_eq!(&bytes[..len], &[0x80, 61, 0]);
        assert_eq!(from_midi(&bytes[..len]), Some(off));
    }

    #[test]
    fn quiet_note_on_stays_a_note_on() {
        let on = EventKind::NoteOn {
            channel: 0,
            key: 1,
            velocity: 0.0,
            note_id: 7,
        };
        assert_eq!(to_midi(&on).unwrap().0, [0x90, 1, 1]);
        let nan = EventKind::NoteOn {
            channel: 0,
            key: 1,
            velocity: f32::NAN,
            note_id: 7,
        };
        assert_eq!(to_midi(&nan).unwrap().0, [0x90, 1, 1]);
    }

    #[test]
    fn note_on_with_zero_velocity_is_note_off() {
        assert!(matches!(
            from_midi(&[0x91, 64, 0]),
            Some(EventKind::NoteOff {
                channel: 1,
                key: 64,
                ..
            })
        ));
    }

    #[test]
    fn short_messages_use_their_real_length() {
        let pc = EventKind::Midi([0xc0, 5, 0]);
        assert_eq!(to_midi(&pc), Some(([0xc0, 5, 0], 2)));
        assert_eq!(from_midi(&[0xc0, 5]), Some(pc));
        assert_eq!(from_midi(&[0xf0, 1, 2, 0xf7]), None);
        assert_eq!(from_midi(&[0xb0, 7]), None);
        assert_eq!(
            from_midi(&[0x90, 0x85, 0x40]),
            None,
            "data byte with the top bit set"
        );
        assert_eq!(
            to_midi(&EventKind::ParamValue {
                param: 0,
                value: 1.0
            }),
            None
        );
    }
}
