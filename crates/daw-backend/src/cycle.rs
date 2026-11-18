use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;

use daw_core::event::{Event, EventKind, EventList, midi};
use daw_core::queue::{Consumer, Producer, spsc};

use crate::stats::SharedStats;
use crate::{BackendConfig, BackendError, MAX_MIDI_INPUTS, MidiInputPort, ProcessCallback};

/// Everything the process callback gets for one audio period.
///
/// Audio slices are all exactly [`frames`](Self::frames) long. Outputs hold
/// garbage on entry; the callback must write every sample.
pub struct BackendCycle<'a> {
    /// Frames in this period. Can differ between cycles.
    pub frames: u32,
    pub sample_rate: f64,
    /// Frames processed before this cycle since [`crate::AudioBackend::start`].
    pub frame_time: u64,
    pub audio_in: &'a [&'a [f32]],
    pub audio_out: &'a mut [&'a mut [f32]],
    /// MIDI received this period, sorted by frame. Note on and off arrive as
    /// `EventKind::NoteOn`/`NoteOff`, other short messages as `EventKind::Midi`.
    /// SysEx is not passed through.
    pub midi_in: &'a EventList,
    /// MIDI received this period on each port made by
    /// [`AudioBackend::add_midi_input`](crate::AudioBackend::add_midi_input),
    /// by slot; [`MAX_MIDI_INPUTS`] lists, empty for
    /// unused slots. Decoded like `midi_in`.
    pub midi_inputs: &'a [EventList],
    /// MIDI to send this period. Starts empty. An event whose frame is past
    /// the period goes out on the period's last frame. Without a MIDI output
    /// nothing is sent or counted. `ParamValue` events are ignored;
    /// malformed `Midi` events (no fixed-length status byte, or a data byte
    /// with bit 7 set) are dropped and counted in
    /// [`BackendStats::midi_dropped`](crate::BackendStats::midi_dropped).
    pub midi_out: &'a mut EventList,
    /// The backend's xrun count so far.
    pub xruns: u64,
}

/// Reusable storage for the per-cycle slice-of-slices, so a cycle can hand out
/// `&[&[f32]]` without allocating.
pub(crate) struct PortBuffers {
    ins: Vec<*const [f32]>,
    outs: Vec<*mut [f32]>,
}

// SAFETY: the pointers are only dereferenced through the references `run`
// hands out, which cannot outlive its borrow; `run` resets them every call.
unsafe impl Send for PortBuffers {}

impl PortBuffers {
    pub fn new(inputs: usize, outputs: usize) -> Self {
        Self {
            ins: Vec::with_capacity(inputs),
            outs: Vec::with_capacity(outputs),
        }
    }

    /// Collects the port slices and calls `f` with them. Takes at most as many
    /// slices as the capacity given to [`new`](Self::new), so it never
    /// allocates. Crate-private: callers must not let the slices escape `f`.
    pub fn run<'s, 'p: 's>(
        &'s mut self,
        ins: impl IntoIterator<Item = &'p [f32]>,
        outs: impl IntoIterator<Item = &'p mut [f32]>,
        f: impl FnOnce(&'s [&'s [f32]], &'s mut [&'s mut [f32]]),
    ) {
        self.ins.clear();
        self.outs.clear();
        let (ci, co) = (self.ins.capacity(), self.outs.capacity());
        self.ins
            .extend(ins.into_iter().take(ci).map(|s| s as *const [f32]));
        self.outs
            .extend(outs.into_iter().take(co).map(|s| s as *mut [f32]));
        // SAFETY: `*const [f32]`/`*mut [f32]` have the same layout as
        // `&[f32]`/`&mut [f32]`. Every pointer came from a reference valid for
        // 'p, which outlives 's; the output slices are disjoint, and `self`
        // stays borrowed for 's so nothing else touches the vectors while the
        // references live.
        let (ins, outs) = unsafe {
            (
                &*(self.ins.as_slice() as *const [*const [f32]] as *const [&'s [f32]]),
                &mut *(self.outs.as_mut_slice() as *mut [*mut [f32]] as *mut [&'s mut [f32]]),
            )
        };
        f(ins, outs);
    }
}

/// What a backend's MIDI ports do within one cycle: hand over the raw
/// messages they received and take the ones to send. [`CycleDriver::run`]
/// does everything else.
pub(crate) trait MidiPorts {
    /// One of the extra input ports ([`MidiInputs`]).
    type Input;

    /// Calls `each` with the frame and bytes of every message the main input
    /// received this cycle. Nothing, without that port.
    fn read_main(&mut self, each: &mut dyn FnMut(u32, &[u8]));

    /// As [`read_main`](Self::read_main), for an extra input.
    fn read(&mut self, port: &mut Self::Input, each: &mut dyn FnMut(u32, &[u8]));

    /// Whether there is an output port. Without one, what the callback
    /// wanted sent is not looked at.
    fn has_output(&self) -> bool;

    /// Sends one message at `frame` of this cycle. False when it did not fit.
    fn write(&mut self, frame: u32, bytes: &[u8]) -> bool;
}

/// The part of a cycle every backend shares: MIDI in through the one codec,
/// the callback, MIDI out, and the counters. A backend owns one while it
/// runs and calls [`run`](Self::run) once per period with its ports.
pub(crate) struct CycleDriver {
    cb: ProcessCallback,
    bufs: PortBuffers,
    midi_in: EventList,
    midi_out: EventList,
    frame_time: u64,
    shared: Arc<SharedStats>,
}

impl CycleDriver {
    pub fn new(cb: ProcessCallback, cfg: &BackendConfig, shared: Arc<SharedStats>) -> Self {
        Self {
            cb,
            bufs: PortBuffers::new(cfg.audio_inputs.into(), cfg.audio_outputs.into()),
            midi_in: EventList::with_capacity(cfg.midi_capacity),
            midi_out: EventList::with_capacity(cfg.midi_capacity),
            frame_time: 0,
            shared,
        }
    }

    /// Frames processed by the cycles so far.
    pub fn frame_time(&self) -> u64 {
        self.frame_time
    }

    /// One period of `frames` frames. Real-time safe.
    ///
    /// Incoming messages that are SysEx or malformed are skipped. Counted in
    /// [`BackendStats::midi_dropped`](crate::BackendStats::midi_dropped):
    /// events a list had no room for, outgoing [`EventKind::Midi`] that is
    /// malformed, and messages the output port did not take. An outgoing
    /// event timed past the period goes out on its last frame.
    pub fn run<'p, M: MidiPorts>(
        &mut self,
        frames: u32,
        sample_rate: f64,
        midi: &mut M,
        extra: &mut MidiInputs<M::Input>,
        audio_in: impl IntoIterator<Item = &'p [f32]>,
        audio_out: impl IntoIterator<Item = &'p mut [f32]>,
    ) {
        let mut dropped = 0u64;
        let mut receive = |list: &mut EventList, frame, bytes: &[u8]| {
            if let Some(kind) = midi::decode(bytes)
                && list.push(Event { frame, kind }).is_err()
            {
                dropped += 1;
            }
        };
        self.midi_in.clear();
        self.midi_out.clear();
        let main = &mut self.midi_in;
        midi.read_main(&mut |frame, bytes| receive(main, frame, bytes));
        extra.begin_cycle();
        for (port, list) in extra.ports.iter_mut().zip(&mut extra.lists) {
            if let Some(port) = port {
                midi.read(port, &mut |frame, bytes| receive(list, frame, bytes));
            }
        }

        let frame_time = self.frame_time;
        let xruns = self.shared.xruns.load(Relaxed);
        let (cb, midi_in, midi_out) = (&mut self.cb, &self.midi_in, &mut self.midi_out);
        let midi_inputs = &extra.lists[..];
        self.bufs.run(audio_in, audio_out, |audio_in, audio_out| {
            cb(&mut BackendCycle {
                frames,
                sample_rate,
                frame_time,
                audio_in,
                audio_out,
                midi_in,
                midi_inputs,
                midi_out,
                xruns,
            })
        });

        if midi.has_output() {
            let last = frames.saturating_sub(1);
            for e in self.midi_out.iter() {
                let sent = match midi::encode(&e.kind) {
                    Some((bytes, len)) => midi.write(e.frame.min(last), &bytes[..len]),
                    // A parameter change has no MIDI form to lose.
                    None => !matches!(e.kind, EventKind::Midi(_)),
                };
                dropped += u64::from(!sent);
            }
        }

        self.frame_time += u64::from(frames);
        self.shared.frames.fetch_add(u64::from(frames), Relaxed);
        self.shared.cycles.fetch_add(1, Relaxed);
        if dropped > 0 {
            self.shared.midi_dropped.fetch_add(dropped, Relaxed);
        }
    }
}

/// A change to the extra MIDI inputs, sent to whoever owns the ports.
pub(crate) enum InputChange<P> {
    Add(usize, P),
    Remove(usize),
}

/// The extra MIDI input ports ([`crate::AudioBackend::add_midi_input`]) and
/// their per-cycle event lists. It moves to the audio thread while running;
/// [`MidiInputsControl`] changes it from the main thread either way.
pub(crate) struct MidiInputs<P> {
    pub ports: Vec<Option<P>>,
    pub lists: Vec<EventList>,
    changes: Consumer<InputChange<P>>,
    removed: Producer<P>,
}

/// The main thread's side of [`MidiInputs`].
pub(crate) struct MidiInputsControl<P> {
    changes: Producer<InputChange<P>>,
    removed: Consumer<P>,
    /// Full names of the ports in use, by slot.
    names: Vec<Option<String>>,
}

/// Room for every slot to be added and removed before the other side looks.
const CHANGE_QUEUE: usize = MAX_MIDI_INPUTS * 2;

/// The other thread still holds changes it has not taken.
fn not_caught_up() -> BackendError {
    BackendError::Other("the audio thread has not caught up with earlier port changes".into())
}

pub(crate) fn midi_inputs<P: Send>(capacity: usize) -> (MidiInputs<P>, MidiInputsControl<P>) {
    let (changes_tx, changes_rx) = spsc(CHANGE_QUEUE);
    let (removed_tx, removed_rx) = spsc(CHANGE_QUEUE);
    let n = MAX_MIDI_INPUTS;
    (
        MidiInputs {
            ports: (0..n).map(|_| None).collect(),
            lists: (0..n).map(|_| EventList::with_capacity(capacity)).collect(),
            changes: changes_rx,
            removed: removed_tx,
        },
        MidiInputsControl {
            changes: changes_tx,
            removed: removed_rx,
            names: vec![None; n],
        },
    )
}

impl<P> MidiInputs<P> {
    /// Applies queued changes and clears the lists. Real-time safe: removed
    /// ports go back to the main thread to be freed.
    pub fn begin_cycle(&mut self) {
        while let Some(change) = self.changes.pop() {
            let (slot, port) = match change {
                InputChange::Add(slot, port) => (slot, Some(port)),
                InputChange::Remove(slot) => (slot, None),
            };
            let old = std::mem::replace(&mut self.ports[slot], port);
            if let Some(old) = old {
                // The queue has room for every slot; should it not, the port
                // is freed here rather than lost.
                let _ = self.removed.push(old);
            }
        }
        for list in &mut self.lists {
            list.clear();
        }
    }
}

fn full() -> BackendError {
    BackendError::Config(format!("at most {MAX_MIDI_INPUTS} MIDI inputs"))
}

impl<P> MidiInputsControl<P> {
    /// Whether a slot is free for another port; the error to give when none is.
    pub fn check_room(&self) -> Result<(), BackendError> {
        if self.names.iter().any(Option::is_none) {
            Ok(())
        } else {
            Err(full())
        }
    }

    /// Hands `port`, whose full name is `name`, to the first free slot. When
    /// there is none, or the owner is behind on earlier changes, the port
    /// comes back with the error.
    pub fn add(&mut self, name: String, port: P) -> Result<MidiInputPort, (BackendError, P)> {
        let Some(slot) = self.names.iter().position(Option::is_none) else {
            return Err((full(), port));
        };
        match self.changes.push(InputChange::Add(slot, port)) {
            Ok(()) => {
                self.names[slot] = Some(name.clone());
                Ok(MidiInputPort { slot, name })
            }
            Err(InputChange::Add(_, port)) => Err((not_caught_up(), port)),
            Err(InputChange::Remove(_)) => unreachable!("pushed an Add"),
        }
    }

    /// Takes the port out of `slot` and returns its full name. The port
    /// comes back through [`take_removed`](Self::take_removed) once the owner
    /// has let go.
    pub fn remove(&mut self, slot: usize) -> Result<String, BackendError> {
        if !self.names.get(slot).is_some_and(Option::is_some) {
            return Err(BackendError::Config(format!(
                "no MIDI input in slot {slot}"
            )));
        }
        self.changes
            .push(InputChange::Remove(slot))
            .map_err(|_| not_caught_up())?;
        Ok(self.names[slot].take().expect("checked above"))
    }

    /// Ports the owner let go of since the last call.
    pub fn take_removed(&mut self) -> impl Iterator<Item = P> + '_ {
        self.removed.drain()
    }

    /// The ports in use.
    pub fn ports(&self) -> Vec<MidiInputPort> {
        self.names
            .iter()
            .enumerate()
            .filter_map(|(slot, name)| {
                Some(MidiInputPort {
                    slot,
                    name: name.clone()?,
                })
            })
            .collect()
    }
}
