//! Conversion between `daw_core` events and CLAP events.

use clack_host::events::event_types::{
    MidiEvent, NoteOffEvent, NoteOnEvent, ParamValueEvent, TransportEvent, TransportFlags,
};
use clack_host::events::io::{OutputEventBuffer, TryPushError};
use clack_host::events::spaces::CoreEventSpace;
use clack_host::events::{EventFlags, EventHeader, Match, Pckn, UnknownEvent};
use clack_host::prelude::*;
use clack_host::utils::{BeatTime, SecondsTime};
use daw_core::event::{Event, EventKind, EventList, NoteId};
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
                out.push(&MidiEvent::new(
                    frame,
                    0,
                    [0x80 | (channel & 0x0f), key & 0x7f, midi_velocity(velocity)],
                ));
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

/// The `daw_core` form of a CLAP event the plugin sent, if there is one.
pub(crate) fn from_clap(e: &UnknownEvent, frame_offset: u32) -> Option<Event> {
    let frame = e.header().time() + frame_offset;
    let kind = match e.as_core_event()? {
        CoreEventSpace::NoteOn(n) => {
            let raw = n.as_raw();
            EventKind::NoteOn {
                channel: to_u8(raw.channel),
                key: to_u8(raw.key),
                #[allow(clippy::cast_possible_truncation)]
                velocity: raw.velocity as f32,
                note_id: raw.note_id,
            }
        }
        CoreEventSpace::NoteOff(n) => {
            let raw = n.as_raw();
            EventKind::NoteOff {
                channel: to_u8(raw.channel),
                key: to_u8(raw.key),
                #[allow(clippy::cast_possible_truncation)]
                velocity: raw.velocity as f32,
                note_id: raw.note_id,
            }
        }
        CoreEventSpace::Midi(m) => EventKind::Midi(m.data()),
        CoreEventSpace::ParamValue(p) => EventKind::ParamValue {
            param: p.param_id()?.get(),
            value: p.value(),
        },
        _ => return None,
    };
    Some(Event { frame, kind })
}

/// Receives a plugin's output events and converts them into a fixed-capacity [`EventList`].
/// Events the list cannot hold, and kinds `daw_core` has no form for, are dropped.
pub(crate) struct Sink<'a> {
    pub(crate) out: &'a mut EventList,
    pub(crate) frame_offset: u32,
}

impl OutputEventBuffer for Sink<'_> {
    fn try_push(&mut self, event: &UnknownEvent) -> Result<(), TryPushError> {
        match from_clap(event, self.frame_offset) {
            Some(e) => self.out.push(e).map_err(|_| TryPushError::new()),
            None => Ok(()),
        }
    }
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

fn pckn(channel: u8, key: u8, note_id: NoteId) -> Pckn {
    let id = if note_id < 0 {
        Match::All
    } else {
        Match::Specific(note_id.unsigned_abs())
    };
    Pckn::new(0u16, u16::from(channel), u16::from(key), id)
}

fn midi_velocity(v: f32) -> u8 {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let v = (v.clamp(0.0, 1.0) * 127.0).round() as u8;
    v
}

fn to_u8(v: i16) -> u8 {
    u8::try_from(v).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
