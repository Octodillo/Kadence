//! Conversion between `daw_core` events and CLAP events.

use clack_host::events::event_types::{
    MidiEvent, NoteOffEvent, NoteOnEvent, ParamValueEvent, TransportEvent, TransportFlags,
};
use clack_host::events::spaces::CoreEventSpace;
use clack_host::events::{EventFlags, EventHeader, Match, Pckn, UnknownEvent};
use clack_host::prelude::*;
use clack_host::utils::{BeatTime, SecondsTime};
use daw_core::event::{Event, EventKind, NoteId};
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
                    [0x90 | (channel & 0x0f), key & 0x7f, midi_velocity(velocity)],
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

/// CLAP transport for a block starting `offset` frames after the context's position.
pub(crate) fn transport(t: &TransportState, offset: u32) -> TransportEvent {
    let mut flags = TransportFlags::HAS_SECONDS_TIMELINE;
    if t.playing {
        flags |= TransportFlags::IS_PLAYING;
    }
    let seconds = if t.sample_rate > 0.0 {
        #[allow(clippy::cast_precision_loss)]
        let samples = (t.position.0 + i64::from(offset)) as f64;
        samples / t.sample_rate
    } else {
        0.0
    };
    let beats = if t.bpm > 0.0 {
        flags |= TransportFlags::HAS_TEMPO | TransportFlags::HAS_BEATS_TIMELINE;
        seconds * t.bpm / 60.0
    } else {
        0.0
    };
    TransportEvent {
        header: EventHeader::new_core(0, EventFlags::empty()),
        flags,
        song_pos_beats: BeatTime::from_float(beats),
        song_pos_seconds: SecondsTime::from_float(seconds),
        tempo: t.bpm,
        tempo_inc: 0.0,
        loop_start_beats: BeatTime::from_int(0),
        loop_end_beats: BeatTime::from_int(0),
        loop_start_seconds: SecondsTime::from_int(0),
        loop_end_seconds: SecondsTime::from_int(0),
        bar_start: BeatTime::from_int(0),
        bar_number: 0,
        time_signature_numerator: 4,
        time_signature_denominator: 4,
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
