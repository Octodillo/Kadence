//! Drives a `MidiPlayer` block by block, like the engine does.

#![allow(dead_code)]

use daw_core::event::{Event, EventKind, EventList};
use daw_core::node::{Node, ProcessContext, TransportState};
use daw_core::time::SampleTime;
use daw_midi::player::{MidiPlayer, PlayerHandle, midi_player};
use daw_midi::sequence::{SeqNote, Sequence};

pub struct Rig {
    pub player: MidiPlayer,
    pub handle: PlayerHandle,
    events_in: EventList,
    events_out: EventList,
}

impl Rig {
    pub fn new() -> Self {
        let (mut player, handle) = midi_player(1024);
        player.prepare(48_000.0, 4096);
        Self {
            player,
            handle,
            events_in: EventList::with_capacity(1024),
            events_out: EventList::with_capacity(1024),
        }
    }

    pub fn with(notes: Vec<SeqNote>) -> Self {
        let mut rig = Self::new();
        rig.handle.set_sequence(Sequence::new(notes)).unwrap();
        rig
    }

    /// Runs one block and returns its output with absolute times.
    pub fn block(
        &mut self,
        pos: i64,
        frames: u32,
        playing: bool,
        input: &[Event],
    ) -> Vec<(i64, EventKind)> {
        self.block_into(pos, frames, playing, input);
        self.events_out
            .iter()
            .map(|e| (pos + e.frame as i64, e.kind))
            .collect()
    }

    /// Runs one block without collecting (no allocation).
    pub fn block_into(&mut self, pos: i64, frames: u32, playing: bool, input: &[Event]) {
        self.events_in.clear();
        for e in input {
            let _ = self.events_in.push(*e);
        }
        self.events_out.clear();
        let transport = TransportState {
            playing,
            position: SampleTime(pos),
            bpm: 120.0,
            sample_rate: 48_000.0,
        };
        let mut audio_out: [&mut [f32]; 0] = [];
        self.player.process(&mut ProcessContext {
            frames,
            transport: &transport,
            audio_in: &[],
            audio_out: &mut audio_out,
            events_in: &self.events_in,
            events_out: &mut self.events_out,
        });
    }

    pub fn out_len(&self) -> usize {
        self.events_out.len()
    }

    /// Plays from `from` to `to` in blocks of the given sizes (cycled).
    pub fn play(&mut self, from: i64, to: i64, sizes: &[u32]) -> Vec<(i64, EventKind)> {
        let mut out = Vec::new();
        let mut pos = from;
        let mut i = 0;
        while pos < to {
            let n = (sizes[i % sizes.len()] as i64).min(to - pos) as u32;
            out.extend(self.block(pos, n, true, &[]));
            pos += n as i64;
            i += 1;
        }
        out
    }
}

pub fn note(start: i64, end: i64, key: u8, uid: u64) -> SeqNote {
    SeqNote {
        start: SampleTime(start),
        end: SampleTime(end),
        key,
        channel: 0,
        velocity: 0.8,
        uid,
    }
}

/// `(time, key, is_on)` for note events, for easy comparison.
pub fn simple(events: &[(i64, EventKind)]) -> Vec<(i64, u8, bool)> {
    events
        .iter()
        .filter_map(|(t, k)| match *k {
            EventKind::NoteOn { key, .. } => Some((*t, key, true)),
            EventKind::NoteOff { key, .. } => Some((*t, key, false)),
            _ => None,
        })
        .collect()
}
