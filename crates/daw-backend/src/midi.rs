//! Conversion between raw MIDI bytes and [`daw_core::event`] events.
//! Everything here is allocation-free and safe on the real-time thread.

use daw_core::event::{Event, EventKind};

/// Length of a MIDI message from its status byte, or `None` for SysEx,
/// undefined system common bytes and data bytes.
pub fn message_len(status: u8) -> Option<usize> {
    match status {
        0x80..=0xBF | 0xE0..=0xEF | 0xF2 => Some(3),
        0xC0..=0xDF | 0xF1 | 0xF3 => Some(2),
        0xF6 | 0xF8..=0xFF => Some(1),
        _ => None,
    }
}

/// Decodes one raw MIDI message at `frame` into an event.
///
/// Note on and note off become `NoteOn`/`NoteOff` with velocity scaled to
/// 0..=1 and no note id; a note on with velocity 0 is a note off. Other
/// messages of one to three bytes become `Midi`, zero-padded. SysEx and
/// malformed messages return `None`.
pub fn decode(frame: u32, bytes: &[u8]) -> Option<Event> {
    let &status = bytes.first()?;
    let len = message_len(status)?;
    if bytes.len() != len || bytes[1..].iter().any(|b| b & 0x80 != 0) {
        return None;
    }
    let channel = status & 0x0F;
    let kind = match (status & 0xF0, len) {
        (0x90, 3) if bytes[2] > 0 => EventKind::NoteOn {
            channel,
            key: bytes[1],
            velocity: f32::from(bytes[2]) / 127.0,
            note_id: -1,
        },
        (0x80 | 0x90, 3) => EventKind::NoteOff {
            channel,
            key: bytes[1],
            velocity: if status & 0xF0 == 0x80 {
                f32::from(bytes[2]) / 127.0
            } else {
                0.0
            },
            note_id: -1,
        },
        _ => {
            let mut raw = [0; 3];
            raw[..len].copy_from_slice(bytes);
            EventKind::Midi(raw)
        }
    };
    Some(Event { frame, kind })
}

/// Encodes an event as raw MIDI into `out`, returning the message length.
/// Returns `None` for events with no MIDI form (`ParamValue`, or a `Midi`
/// event whose first byte is not a valid status byte).
pub fn encode(kind: &EventKind, out: &mut [u8; 3]) -> Option<usize> {
    match *kind {
        EventKind::NoteOn {
            channel,
            key,
            velocity,
            ..
        } => {
            // Velocity 0 would mean note off, so a quiet note on stays at 1.
            *out = [
                0x90 | (channel & 0x0F),
                key & 0x7F,
                to_7bit(velocity).max(1),
            ];
            Some(3)
        }
        EventKind::NoteOff {
            channel,
            key,
            velocity,
            ..
        } => {
            *out = [0x80 | (channel & 0x0F), key & 0x7F, to_7bit(velocity)];
            Some(3)
        }
        EventKind::Midi(raw) => {
            let len = message_len(raw[0])?;
            *out = raw;
            Some(len)
        }
        EventKind::ParamValue { .. } => None,
    }
}

fn to_7bit(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 127.0).round() as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kind(bytes: &[u8]) -> Option<EventKind> {
        decode(7, bytes).map(|e| {
            assert_eq!(e.frame, 7);
            e.kind
        })
    }

    #[test]
    fn note_on_and_off() {
        assert_eq!(
            kind(&[0x93, 60, 127]),
            Some(EventKind::NoteOn {
                channel: 3,
                key: 60,
                velocity: 1.0,
                note_id: -1
            })
        );
        assert_eq!(
            kind(&[0x80, 61, 0]),
            Some(EventKind::NoteOff {
                channel: 0,
                key: 61,
                velocity: 0.0,
                note_id: -1
            })
        );
    }

    #[test]
    fn note_on_velocity_zero_is_note_off() {
        assert!(matches!(
            kind(&[0x9F, 10, 0]),
            Some(EventKind::NoteOff {
                channel: 15,
                key: 10,
                ..
            })
        ));
    }

    #[test]
    fn short_messages_pass_through_padded() {
        assert_eq!(kind(&[0xB0, 7, 100]), Some(EventKind::Midi([0xB0, 7, 100])));
        assert_eq!(kind(&[0xC2, 5]), Some(EventKind::Midi([0xC2, 5, 0])));
        assert_eq!(kind(&[0xF8]), Some(EventKind::Midi([0xF8, 0, 0])));
    }

    #[test]
    fn rejects_sysex_and_malformed() {
        assert_eq!(kind(&[0xF0, 1, 2, 0xF7]), None);
        assert_eq!(kind(&[]), None);
        assert_eq!(kind(&[60, 100]), None, "data byte as status");
        assert_eq!(kind(&[0x90, 60]), None, "truncated");
        assert_eq!(kind(&[0x90, 60, 0x80]), None, "status byte in data");
    }

    #[test]
    fn encode_round_trips() {
        for msg in [[0x91, 64, 100], [0x85, 1, 2], [0xB3, 1, 2], [0xE0, 0, 64]] {
            let ev = decode(0, &msg).unwrap();
            let mut out = [0; 3];
            assert_eq!(encode(&ev.kind, &mut out), Some(3));
            assert_eq!(out, msg);
        }
        let mut out = [0; 3];
        let pc = decode(0, &[0xC1, 9]).unwrap();
        assert_eq!(encode(&pc.kind, &mut out), Some(2));
        assert_eq!(&out[..2], &[0xC1, 9]);
    }

    #[test]
    fn encode_clamps_and_keeps_quiet_notes_on() {
        let mut out = [0; 3];
        let quiet = EventKind::NoteOn {
            channel: 0,
            key: 60,
            velocity: 0.0,
            note_id: 3,
        };
        encode(&quiet, &mut out);
        assert_eq!(out, [0x90, 60, 1]);
        let loud = EventKind::NoteOff {
            channel: 0,
            key: 60,
            velocity: 4.0,
            note_id: -1,
        };
        encode(&loud, &mut out);
        assert_eq!(out, [0x80, 60, 127]);
        let param = EventKind::ParamValue {
            param: 1,
            value: 0.5,
        };
        assert_eq!(encode(&param, &mut out), None);
        assert_eq!(encode(&EventKind::Midi([0x40, 0, 0]), &mut out), None);
    }
}
