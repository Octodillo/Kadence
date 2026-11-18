//! The conversion between raw MIDI bytes and [`EventKind`], and the one rule
//! for what a well-formed message is. Everything that reads the bytes of a
//! message, takes them in or hands them on (the audio backends, the plugin
//! formats, the built-in instruments, the player and the recorder) goes
//! through here. Code that only builds a message from known parts, or looks
//! at nothing but a status byte's channel, does not need to.
//!
//! A message is well formed when its first byte is a status byte with a
//! fixed length (so not SysEx) and its data bytes have bit 7 clear. Anything
//! else has no meaning a receiver would agree on, and is dropped: `decode`,
//! `decode_padded` and `encode` return `None` for it.
//!
//! Nothing here allocates.

use super::{EventKind, all_notes_off};

/// Length of a MIDI message from its status byte, or `None` for SysEx,
/// the undefined system common bytes (0xF4, 0xF5) and data bytes. Every
/// real-time byte is one byte long, the two undefined ones included.
pub const fn message_len(status: u8) -> Option<usize> {
    match status {
        0x80..=0xBF | 0xE0..=0xEF | 0xF2 => Some(3),
        0xC0..=0xDF | 0xF1 | 0xF3 => Some(2),
        0xF6 | 0xF8..=0xFF => Some(1),
        _ => None,
    }
}

/// Decodes one raw MIDI message, which must be exactly as long as its status
/// byte says.
///
/// Note on and note off become `NoteOn`/`NoteOff` with velocity scaled to
/// 0..=1 and no note id; a note on with velocity 0 is a note off. Other
/// messages become `Midi`, zero-padded.
pub fn decode(bytes: &[u8]) -> Option<EventKind> {
    let &status = bytes.first()?;
    let len = message_len(status)?;
    if bytes.len() != len || bytes[1..].iter().any(|b| b & 0x80 != 0) {
        return None;
    }
    let channel = status & 0x0F;
    Some(match status & 0xF0 {
        0x90 if bytes[2] > 0 => EventKind::NoteOn {
            channel,
            key: bytes[1],
            velocity: f32::from(bytes[2]) / 127.0,
            note_id: -1,
        },
        0x80 | 0x90 => EventKind::NoteOff {
            channel,
            key: Some(bytes[1]),
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
    })
}

/// Decodes the bytes of an [`EventKind::Midi`], whose message may be shorter
/// than the three bytes it is stored in. This is how a consumer that only
/// knows notes reads raw MIDI: a note comes back as `NoteOn`/`NoteOff`.
pub fn decode_padded(raw: [u8; 3]) -> Option<EventKind> {
    decode(&raw[..message_len(raw[0])?])
}

/// Encodes an event as raw MIDI: the bytes, zero-padded, and how many of them
/// are the message. `None` for an event with no MIDI form (`ParamValue`) and
/// for a `Midi` that is not well formed.
///
/// A note on never encodes with velocity 0, which would mean note off, and a
/// wildcard note off becomes all notes off, the nearest MIDI has.
pub fn encode(kind: &EventKind) -> Option<([u8; 3], usize)> {
    match *kind {
        EventKind::NoteOn {
            channel,
            key,
            velocity,
            ..
        } => Some((
            [
                0x90 | (channel & 0x0F),
                key & 0x7F,
                to_7bit(velocity).max(1),
            ],
            3,
        )),
        EventKind::NoteOff {
            channel,
            key: Some(key),
            velocity,
            ..
        } => Some(([0x80 | (channel & 0x0F), key & 0x7F, to_7bit(velocity)], 3)),
        EventKind::NoteOff {
            channel, key: None, ..
        } => Some((all_notes_off(channel), 3)),
        EventKind::Midi(raw) => {
            let len = message_len(raw[0])?;
            // A data byte with bit 7 set is a status byte to the receiver,
            // which then misparses the rest of the stream.
            if raw[1..len].iter().any(|b| b & 0x80 != 0) {
                return None;
            }
            let mut out = [0; 3];
            out[..len].copy_from_slice(&raw[..len]);
            Some((out, len))
        }
        EventKind::ParamValue { .. } => None,
    }
}

/// A 0..=1 value (a velocity, a pressure) as a MIDI data byte. Out of range
/// clamps; NaN is 0.
pub fn to_7bit(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 127.0).round() as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encoded(kind: EventKind) -> Option<Vec<u8>> {
        encode(&kind).map(|(bytes, len)| bytes[..len].to_vec())
    }

    #[test]
    fn note_on_and_off() {
        assert_eq!(
            decode(&[0x93, 60, 127]),
            Some(EventKind::NoteOn {
                channel: 3,
                key: 60,
                velocity: 1.0,
                note_id: -1
            })
        );
        assert_eq!(
            decode(&[0x80, 61, 0]),
            Some(EventKind::NoteOff {
                channel: 0,
                key: Some(61),
                velocity: 0.0,
                note_id: -1
            })
        );
    }

    #[test]
    fn note_on_velocity_zero_is_note_off() {
        assert_eq!(
            decode(&[0x9F, 10, 0]),
            Some(EventKind::NoteOff {
                channel: 15,
                key: Some(10),
                velocity: 0.0,
                note_id: -1
            })
        );
    }

    #[test]
    fn short_messages_pass_through_padded() {
        assert_eq!(
            decode(&[0xB0, 7, 100]),
            Some(EventKind::Midi([0xB0, 7, 100]))
        );
        assert_eq!(decode(&[0xC2, 5]), Some(EventKind::Midi([0xC2, 5, 0])));
        assert_eq!(decode(&[0xF8]), Some(EventKind::Midi([0xF8, 0, 0])));
    }

    #[test]
    fn rejects_sysex_and_malformed() {
        assert_eq!(decode(&[0xF0, 1, 2, 0xF7]), None);
        assert_eq!(decode(&[]), None);
        assert_eq!(decode(&[60, 100]), None, "data byte as status");
        assert_eq!(decode(&[0x90, 60]), None, "truncated");
        assert_eq!(decode(&[0xC0, 5, 0]), None, "too long");
        assert_eq!(decode(&[0x90, 60, 0x80]), None, "status byte in data");
        assert_eq!(decode(&[0x90, 0x85, 0x40]), None, "status byte in data");
    }

    #[test]
    fn padded_bytes_decode_by_their_real_length() {
        assert_eq!(
            decode_padded([0x91, 64, 100]),
            decode(&[0x91, 64, 100]),
            "a note comes back as a note"
        );
        assert_eq!(
            decode_padded([0xC0, 5, 0xFF]),
            Some(EventKind::Midi([0xC0, 5, 0]))
        );
        assert_eq!(decode_padded([0x90, 60, 0x80]), None);
        assert_eq!(decode_padded([0x40, 0, 0]), None);
    }

    #[test]
    fn decoding_and_encoding_agree_on_what_is_well_formed() {
        for status in 0..=255u8 {
            for data in [[0, 0], [0x7F, 0x7F], [0x80, 0], [0, 0x80], [0xFF, 0xFF]] {
                let raw = [status, data[0], data[1]];
                let decoded = decode_padded(raw);
                assert_eq!(
                    decoded.is_some(),
                    encode(&EventKind::Midi(raw)).is_some(),
                    "{raw:02X?}"
                );
                // What decodes encodes back to the same message, but for a
                // note on with velocity 0, which is the note off it means.
                if let Some(kind) = decoded {
                    let len = message_len(status).unwrap();
                    let mut same = raw;
                    if status & 0xF0 == 0x90 && raw[2] == 0 {
                        same[0] = 0x80 | (status & 0x0F);
                    }
                    assert_eq!(encoded(kind).unwrap(), same[..len], "{raw:02X?}");
                }
            }
        }
    }

    #[test]
    fn encode_round_trips() {
        for msg in [[0x91, 64, 100], [0x85, 1, 2], [0xB3, 1, 2], [0xE0, 0, 64]] {
            assert_eq!(encoded(decode(&msg).unwrap()), Some(msg.to_vec()));
        }
        assert_eq!(encoded(decode(&[0xC1, 9]).unwrap()), Some(vec![0xC1, 9]));
    }

    #[test]
    fn encode_clamps_and_keeps_quiet_notes_on() {
        let on = |velocity| EventKind::NoteOn {
            channel: 0,
            key: 60,
            velocity,
            note_id: 3,
        };
        assert_eq!(encoded(on(0.0)), Some(vec![0x90, 60, 1]));
        assert_eq!(encoded(on(f32::NAN)), Some(vec![0x90, 60, 1]));
        assert_eq!(encoded(on(9.0)), Some(vec![0x90, 60, 127]));
        let loud = EventKind::NoteOff {
            channel: 0,
            key: Some(60),
            velocity: 4.0,
            note_id: -1,
        };
        assert_eq!(encoded(loud), Some(vec![0x80, 60, 127]));
    }

    #[test]
    fn a_wildcard_note_off_is_all_notes_off() {
        let any = EventKind::NoteOff {
            channel: 9,
            key: None,
            velocity: 1.0,
            note_id: 2,
        };
        assert_eq!(encoded(any), Some(vec![0xB9, 123, 0]));
    }

    #[test]
    fn encode_has_nothing_for_parameters_and_malformed_bytes() {
        let param = EventKind::ParamValue {
            param: 1,
            value: 0.5,
        };
        assert_eq!(encode(&param), None);
        for raw in [
            [0x40, 0, 0],
            [0xF0, 1, 2],
            [0xB0, 7, 200],
            [0x90, 0x80, 1],
            [0xE0, 0, 0xFF],
            [0xC0, 0x85, 0],
        ] {
            assert_eq!(encode(&EventKind::Midi(raw)), None, "{raw:02X?}");
        }
        // Bytes past the message length are padding: not checked, not sent.
        assert_eq!(
            encode(&EventKind::Midi([0xC0, 5, 0xFF])),
            Some(([0xC0, 5, 0], 2))
        );
        assert_eq!(
            encode(&EventKind::Midi([0xF8, 0xFF, 0xFF])),
            Some(([0xF8, 0, 0], 1))
        );
    }
}
