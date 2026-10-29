//! Event plumbing shared by the devices.

use daw_core::event::{EventKind, NoteId, ParamId};
use daw_core::node::ProcessContext;

/// Note-related messages after normalizing contract events and raw MIDI.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) enum Note {
    On {
        key: u8,
        velocity: f32,
        id: NoteId,
    },
    Off {
        key: u8,
        id: NoteId,
    },
    AllOff,
    /// Pitch bend in `-1..=1`.
    Bend(f32),
}

pub(crate) fn note(kind: &EventKind) -> Option<Note> {
    match *kind {
        EventKind::NoteOn {
            key,
            velocity,
            note_id,
            ..
        } if velocity > 0.0 => Some(Note::On {
            key: key.min(127),
            velocity: velocity.min(1.0),
            id: note_id,
        }),
        EventKind::NoteOn { key, note_id, .. } | EventKind::NoteOff { key, note_id, .. } => {
            Some(Note::Off { key, id: note_id })
        }
        EventKind::Midi([status, d1, d2]) => match status & 0xF0 {
            0x90 if d2 > 0 => Some(Note::On {
                key: d1 & 0x7F,
                velocity: (d2 & 0x7F) as f32 / 127.0,
                id: -1,
            }),
            0x80 | 0x90 => Some(Note::Off {
                key: d1 & 0x7F,
                id: -1,
            }),
            0xB0 if d1 == 120 || d1 == 123 => Some(Note::AllOff),
            0xE0 => {
                let v = (((d2 as i32 & 0x7F) << 7) | (d1 as i32 & 0x7F)) - 8192;
                Some(Note::Bend(v as f32 / 8192.0))
            }
            _ => None,
        },
        EventKind::ParamValue { .. } => None,
    }
}

/// A device whose processing is split at incoming event frames, so every
/// event (parameter change or note) takes effect sample-accurately.
pub(crate) trait Segmented {
    fn param_event(&mut self, param: ParamId, value: f64);
    fn note_event(&mut self, _note: Note) {}
    /// Render frames `start..end` of the buffer.
    fn render(&mut self, ctx: &mut ProcessContext, start: usize, end: usize);
}

pub(crate) fn process_segmented<T: Segmented>(dev: &mut T, ctx: &mut ProcessContext) {
    let n = frames(ctx);
    let events = ctx.events_in;
    let mut pos = 0;
    for ev in events.iter() {
        let at = (ev.frame as usize).min(n);
        if at > pos {
            dev.render(ctx, pos, at);
            pos = at;
        }
        match ev.kind {
            EventKind::ParamValue { param, value } => dev.param_event(param, value),
            ref kind => {
                if let Some(note) = note(kind) {
                    dev.note_event(note);
                }
            }
        }
    }
    if pos < n {
        dev.render(ctx, pos, n);
    }
}

/// Frames to process: `ctx.frames`, but never more than the buffers hold.
pub(crate) fn frames(ctx: &ProcessContext) -> usize {
    let mut n = ctx.frames as usize;
    for b in ctx.audio_in.iter() {
        n = n.min(b.len());
    }
    for b in ctx.audio_out.iter() {
        n = n.min(b.len());
    }
    n
}

/// Input sample for a channel; mono input feeds both sides, no input is silence.
#[inline]
pub(crate) fn input(ctx: &ProcessContext, channel: usize, frame: usize) -> f32 {
    match ctx.audio_in.get(channel).or_else(|| ctx.audio_in.first()) {
        Some(b) => b[frame],
        None => 0.0,
    }
}

/// Write a stereo frame to however many outputs the host gave us.
#[inline]
pub(crate) fn output(ctx: &mut ProcessContext, frame: usize, l: f32, r: f32) {
    match ctx.audio_out {
        [] => {}
        [mono] => mono[frame] = 0.5 * (l + r),
        [left, right, rest @ ..] => {
            left[frame] = l;
            right[frame] = r;
            for b in rest {
                b[frame] = 0.0;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn midi_normalization() {
        assert_eq!(
            note(&EventKind::Midi([0x91, 60, 127])),
            Some(Note::On {
                key: 60,
                velocity: 1.0,
                id: -1
            })
        );
        assert_eq!(
            note(&EventKind::Midi([0x90, 60, 0])),
            Some(Note::Off { key: 60, id: -1 })
        );
        assert_eq!(
            note(&EventKind::Midi([0x80, 61, 10])),
            Some(Note::Off { key: 61, id: -1 })
        );
        assert_eq!(note(&EventKind::Midi([0xB0, 123, 0])), Some(Note::AllOff));
        assert_eq!(
            note(&EventKind::Midi([0xE0, 0, 0x40])),
            Some(Note::Bend(0.0))
        );
        assert_eq!(note(&EventKind::Midi([0xE0, 0, 0])), Some(Note::Bend(-1.0)));
        let off = EventKind::NoteOn {
            channel: 0,
            key: 5,
            velocity: 0.0,
            note_id: 3,
        };
        assert_eq!(note(&off), Some(Note::Off { key: 5, id: 3 }));
    }
}
