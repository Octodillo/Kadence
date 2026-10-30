//! Conversion between `daw_core` events and CLAP events.

use clack_host::events::event_types::{
    MidiEvent, NoteOffEvent, NoteOnEvent, ParamValueEvent, TransportEvent, TransportFlags,
};
use clack_host::events::io::{OutputEventBuffer, TryPushError};
use clack_host::events::spaces::CoreEventSpace;
use clack_host::events::{EventFlags, EventHeader, Match, Pckn, UnknownEvent};
use clack_host::prelude::*;
use clack_host::utils::{BeatTime, SecondsTime};
use daw_core::event::{Event, EventKind, EventList, NoteId, all_notes_off};
use daw_core::node::TransportState;

/// Pushes the CLAP form of `e`, at `frame`, into `out`. Notes become MIDI when the plugin's note
/// port does not speak the CLAP note dialect.
pub(crate) fn push_clap(out: &mut EventBuffer, e: &Event, frame: u32, notes_as_midi: bool) {
    match e.kind {
        EventKind::NoteOn {
            channel,
            key,
            velocity,
            note_id,
        } => {
            if notes_as_midi {
                out.push(&MidiEvent::new(
                    frame,
                    0,
                    // Velocity 0 would turn a quiet note on into a note off.
                    [
                        0x90 | (channel & 0x0f),
                        key & 0x7f,
                        midi_velocity(velocity).max(1),
                    ],
                ));
            } else {
                out.push(&NoteOnEvent::new(
                    frame,
                    pckn(channel, key, note_id),
                    f64::from(velocity),
                ));
            }
        }
        EventKind::NoteOff {
            channel,
            key,
            velocity,
            note_id,
        } => {
            if notes_as_midi {
                let data = match key {
                    Some(key) => [0x80 | (channel & 0x0f), key & 0x7f, midi_velocity(velocity)],
                    None => all_notes_off(channel),
                };
                out.push(&MidiEvent::new(frame, 0, data));
            } else {
                out.push(&NoteOffEvent::new(
                    frame,
                    pckn(channel, key, note_id),
                    f64::from(velocity),
                ));
            }
        }
        EventKind::Midi(data) => out.push(&MidiEvent::new(frame, 0, data)),
        EventKind::ParamValue { param, value } => out.push(&ParamValueEvent::new(
            frame,
            ClapId::new(param),
            Pckn::match_all(),
            value,
        )),
    }
}

/// Receives a plugin's output events and converts them into a fixed-capacity [`EventList`].
/// Events the list cannot hold, and kinds `daw_core` has no form for, are dropped.
pub(crate) struct Sink<'a> {
    pub(crate) out: &'a mut EventList,
    pub(crate) frame_offset: u32,
}

impl OutputEventBuffer for Sink<'_> {
    fn try_push(&mut self, event: &UnknownEvent) -> Result<(), TryPushError> {
        push_daw(self.out, event, self.frame_offset).map_err(|()| TryPushError::new())
    }
}

/// Pushes the `daw_core` form of a CLAP event the plugin sent into `out`. Fails when `out` is
/// full. Kinds `daw_core` has no form for are skipped.
pub(crate) fn push_daw(out: &mut EventList, e: &UnknownEvent, frame_offset: u32) -> Result<(), ()> {
    let frame = e.header().time() + frame_offset;
    let mut push = |kind| out.push(Event { frame, kind }).map_err(|_| ());
    match e.as_core_event() {
        Some(CoreEventSpace::NoteOn(n)) => {
            let raw = n.as_raw();
            // A note on for "any key" or "any channel" names no note to start.
            let (Ok(channel), Ok(key)) = (u8::try_from(raw.channel), u8::try_from(raw.key)) else {
                return Ok(());
            };
            push(EventKind::NoteOn {
                channel,
                key,
                #[allow(clippy::cast_possible_truncation)]
                velocity: raw.velocity as f32,
                note_id: raw.note_id,
            })
        }
        Some(CoreEventSpace::NoteOff(n)) => {
            let raw = n.as_raw();
            note_off(raw.channel, raw.key, raw.velocity, raw.note_id, &mut push)
        }
        // Nothing downstream can cut a note short more abruptly than a note off.
        Some(CoreEventSpace::NoteChoke(n)) => {
            let raw = n.as_raw();
            note_off(raw.channel, raw.key, 0.0, raw.note_id, &mut push)
        }
        Some(CoreEventSpace::Midi(m)) => push(EventKind::Midi(m.data())),
        Some(CoreEventSpace::ParamValue(p)) => match p.param_id() {
            Some(param) => push(EventKind::ParamValue {
                param: param.get(),
                value: p.value(),
            }),
            None => Ok(()),
        },
        _ => Ok(()),
    }
}

/// A note off, where -1 for the channel or key means every channel or key. "Any key" maps to
/// `key: None`; `daw_core` note offs always name a channel, so "any channel" becomes one event per
/// channel.
fn note_off(
    channel: i16,
    key: i16,
    velocity: f64,
    note_id: NoteId,
    push: &mut impl FnMut(EventKind) -> Result<(), ()>,
) -> Result<(), ()> {
    let channels = match u8::try_from(channel) {
        Ok(c) => c..c.saturating_add(1),
        Err(_) => 0..16,
    };
    for channel in channels {
        push(EventKind::NoteOff {
            channel,
            key: u8::try_from(key).ok(),
            #[allow(clippy::cast_possible_truncation)]
            velocity: velocity as f32,
            note_id,
        })?;
    }
    Ok(())
}

/// CLAP transport for a block starting `offset` frames after the context's position.
pub(crate) fn transport(t: &TransportState, offset: u32) -> TransportEvent {
    let t = t.advanced(offset);
    let m = &t.music;
    let mut flags = TransportFlags::HAS_SECONDS_TIMELINE;
    if t.playing {
        flags |= TransportFlags::IS_PLAYING;
    }
    let seconds = if t.sample_rate > 0.0 {
        #[allow(clippy::cast_precision_loss)]
        let samples = t.position.0 as f64;
        samples / t.sample_rate
    } else {
        0.0
    };
    if t.bpm > 0.0 {
        flags |= TransportFlags::HAS_TEMPO
            | TransportFlags::HAS_BEATS_TIMELINE
            | TransportFlags::HAS_TIME_SIGNATURE;
    }
    TransportEvent {
        header: EventHeader::new_core(0, EventFlags::empty()),
        flags,
        song_pos_beats: BeatTime::from_float(m.beats),
        song_pos_seconds: SecondsTime::from_float(seconds),
        tempo: t.bpm,
        tempo_inc: 0.0,
        loop_start_beats: BeatTime::from_int(0),
        loop_end_beats: BeatTime::from_int(0),
        loop_start_seconds: SecondsTime::from_int(0),
        loop_end_seconds: SecondsTime::from_int(0),
        bar_start: BeatTime::from_float(m.bar_start),
        #[allow(clippy::cast_possible_truncation)]
        bar_number: m.bar.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
        time_signature_numerator: u16::from(m.numerator),
        time_signature_denominator: u16::from(m.denominator),
    }
}

/// A CLAP note address. `key: None` and a negative `note_id` match every key or note.
fn pckn(channel: u8, key: impl Into<Option<u8>>, note_id: NoteId) -> Pckn {
    let id = if note_id < 0 {
        Match::All
    } else {
        Match::Specific(note_id.unsigned_abs())
    };
    let key = match key.into() {
        Some(key) => Match::Specific(u16::from(key)),
        None => Match::All,
    };
    Pckn::new(0u16, u16::from(channel), key, id)
}

fn midi_velocity(v: f32) -> u8 {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let v = (v.clamp(0.0, 1.0) * 127.0).round() as u8;
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use clack_host::events::event_types::NoteChokeEvent;

    #[test]
    fn transport_has_bars_beats_and_time_signature() {
        use daw_core::time::{Meter, PPQ, SampleTime, SegmentTempoMap, TickTime, Timeline};
        // 120 bpm, then 160 from beat 4, in 3/4.
        let mut tempo = SegmentTempoMap::new(120.0);
        tempo.set_tempo(TickTime(4 * PPQ), 160.0, false);
        let tl = Timeline::new(tempo, Meter::constant(3, 4));
        let t = TransportState::at(&tl, true, SampleTime(96_000), 48_000.0);
        // 18000 frames later is one beat at 160: quarter 5, bar 1 (from 3).
        let e = transport(&t, 18_000);
        assert!((e.song_pos_beats.to_float() - 5.0).abs() < 1e-6);
        assert!((e.song_pos_seconds.to_float() - 2.375).abs() < 1e-6);
        assert_eq!(e.tempo, 160.0);
        assert_eq!(e.bar_number, 1);
        assert!((e.bar_start.to_float() - 3.0).abs() < 1e-6);
        assert_eq!(
            (e.time_signature_numerator, e.time_signature_denominator),
            (3, 4)
        );
        assert!(e.flags.contains(
            TransportFlags::IS_PLAYING
                | TransportFlags::HAS_TEMPO
                | TransportFlags::HAS_BEATS_TIMELINE
                | TransportFlags::HAS_TIME_SIGNATURE
        ));
    }

    #[test]
    fn quiet_notes_sent_as_midi_stay_note_ons() {
        let mut buf = EventBuffer::with_capacity(1);
        let on = Event {
            frame: 0,
            kind: EventKind::NoteOn {
                channel: 2,
                key: 60,
                velocity: 0.001,
                note_id: -1,
            },
        };
        push_clap(&mut buf, &on, 0, true);
        let Some(CoreEventSpace::Midi(m)) = buf.get(0).and_then(|e| e.as_core_event()) else {
            panic!("expected a MIDI event");
        };
        assert_eq!(m.data(), [0x92, 60, 1]);
    }

    fn converted(e: &UnknownEvent) -> Vec<EventKind> {
        let mut out = EventList::with_capacity(32);
        push_daw(&mut out, e, 0).unwrap();
        out.iter().map(|e| e.kind).collect()
    }

    fn note_off(channel: i16, key: i16, note_id: i32) -> EventKind {
        let mut buf = EventBuffer::with_capacity(1);
        buf.push(&NoteOffEvent::new(
            0,
            Pckn::from_raw(0, channel, key, note_id),
            0.5,
        ));
        let kinds = converted(buf.get(0).unwrap());
        assert_eq!(kinds.len(), 1, "{kinds:?}");
        kinds[0]
    }

    #[test]
    fn wildcard_note_offs_release_every_matching_note() {
        // Any key on one channel: a wildcard note off on that channel.
        let mut buf = EventBuffer::with_capacity(4);
        buf.push(&NoteOffEvent::new(0, Pckn::from_raw(0, 3, -1, -1), 0.0));
        assert_eq!(
            converted(buf.get(0).unwrap()),
            [EventKind::NoteOff {
                channel: 3,
                key: None,
                velocity: 0.0,
                note_id: -1
            }]
        );

        // One key on any channel: a note off on every channel.
        buf.clear();
        buf.push(&NoteOffEvent::new(0, Pckn::from_raw(0, -1, 60, 7), 0.0));
        let kinds = converted(buf.get(0).unwrap());
        assert_eq!(kinds.len(), 16);
        for (ch, kind) in (0u8..).zip(&kinds) {
            assert!(
                matches!(kind, EventKind::NoteOff { channel, key: Some(60), note_id: 7, .. } if *channel == ch),
                "{kind:?}"
            );
        }

        // Everything: a wildcard note off on every channel.
        buf.clear();
        buf.push(&NoteChokeEvent::new(0, Pckn::match_all()));
        let kinds = converted(buf.get(0).unwrap());
        assert_eq!(kinds.len(), 16);
        for (ch, kind) in (0u8..).zip(&kinds) {
            assert_eq!(
                *kind,
                EventKind::NoteOff {
                    channel: ch,
                    key: None,
                    velocity: 0.0,
                    note_id: -1
                }
            );
        }
    }

    /// Sends `kind` to a plugin and returns the CLAP event it gets.
    fn sent(kind: EventKind, notes_as_midi: bool) -> EventBuffer {
        let mut buf = EventBuffer::with_capacity(1);
        push_clap(&mut buf, &Event { frame: 0, kind }, 0, notes_as_midi);
        buf
    }

    #[test]
    fn wildcard_note_offs_reach_clap_plugins_as_wildcards() {
        // "Release note 7, whatever its key", from one plugin's output to the next plugin.
        let mut out = EventBuffer::with_capacity(1);
        out.push(&NoteOffEvent::new(0, Pckn::from_raw(0, 2, -1, 7), 0.25));
        let kinds = converted(out.get(0).unwrap());
        assert_eq!(kinds.len(), 1, "{kinds:?}");

        let buf = sent(kinds[0], false);
        let Some(CoreEventSpace::NoteOff(n)) = buf.get(0).and_then(|e| e.as_core_event()) else {
            panic!("expected a CLAP note off");
        };
        let raw = n.as_raw();
        assert_eq!((raw.channel, raw.key, raw.note_id), (2, -1, 7));
        assert_eq!(raw.velocity, 0.25);
    }

    #[test]
    fn wildcard_note_offs_become_all_notes_off_for_midi_ports() {
        let off = EventKind::NoteOff {
            channel: 5,
            key: None,
            velocity: 1.0,
            note_id: 7,
        };
        let buf = sent(off, true);
        let Some(CoreEventSpace::Midi(m)) = buf.get(0).and_then(|e| e.as_core_event()) else {
            panic!("expected a MIDI event");
        };
        assert_eq!(m.data(), [0xb5, 123, 0]);
    }

    #[test]
    fn specific_note_offs_and_chokes_stay_single() {
        assert!(matches!(
            note_off(2, 64, 9),
            EventKind::NoteOff {
                channel: 2,
                key: Some(64),
                note_id: 9,
                ..
            }
        ));
        let mut buf = EventBuffer::with_capacity(1);
        buf.push(&NoteChokeEvent::new(0, Pckn::from_raw(0, 1, 50, -1)));
        let kinds = converted(buf.get(0).unwrap());
        assert!(
            matches!(
                kinds[..],
                [EventKind::NoteOff {
                    channel: 1,
                    key: Some(50),
                    note_id: -1,
                    ..
                }]
            ),
            "{kinds:?}"
        );
    }

    #[test]
    fn wildcard_note_ons_are_dropped() {
        let mut buf = EventBuffer::with_capacity(1);
        buf.push(&NoteOnEvent::new(0, Pckn::from_raw(0, 0, -1, -1), 1.0));
        assert!(converted(buf.get(0).unwrap()).is_empty());
    }
}
