use daw_core::event::EventList;
use daw_core::queue::{Consumer, Producer, spsc};

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
    /// by slot; [`MAX_MIDI_INPUTS`](crate::MAX_MIDI_INPUTS) lists, empty for
    /// unused slots. Decoded like `midi_in`.
    pub midi_inputs: &'a [EventList],
    /// MIDI to send this period. Starts empty. `ParamValue` events are ignored;
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
    pub fn run<'s>(
        &'s mut self,
        ins: impl IntoIterator<Item = &'s [f32]>,
        outs: impl IntoIterator<Item = &'s mut [f32]>,
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
        // 's, the output slices are disjoint, and `self` stays borrowed for 's
        // so nothing else touches the vectors while the references live.
        let (ins, outs) = unsafe {
            (
                &*(self.ins.as_slice() as *const [*const [f32]] as *const [&'s [f32]]),
                &mut *(self.outs.as_mut_slice() as *mut [*mut [f32]] as *mut [&'s mut [f32]]),
            )
        };
        f(ins, outs);
    }
}

/// Pushes into an event list, counting drops instead of failing.
#[inline]
pub(crate) fn push_or_count(list: &mut EventList, e: daw_core::event::Event, dropped: &mut u64) {
    if list.push(e).is_err() {
        *dropped += 1;
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
const CHANGE_QUEUE: usize = crate::MAX_MIDI_INPUTS * 2;

pub(crate) fn midi_inputs<P: Send>(capacity: usize) -> (MidiInputs<P>, MidiInputsControl<P>) {
    let (changes_tx, changes_rx) = spsc(CHANGE_QUEUE);
    let (removed_tx, removed_rx) = spsc(CHANGE_QUEUE);
    let n = crate::MAX_MIDI_INPUTS;
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

impl<P> MidiInputsControl<P> {
    /// The first free slot.
    pub fn free_slot(&self) -> Option<usize> {
        self.names.iter().position(Option::is_none)
    }

    pub fn is_used(&self, slot: usize) -> bool {
        self.names.get(slot).is_some_and(Option::is_some)
    }

    /// Hands `port` to slot `slot`. The caller checked the slot is free.
    pub fn add(&mut self, slot: usize, name: String, port: P) -> Result<(), P> {
        match self.changes.push(InputChange::Add(slot, port)) {
            Ok(()) => {
                self.names[slot] = Some(name);
                Ok(())
            }
            Err(InputChange::Add(_, port)) => Err(port),
            Err(InputChange::Remove(_)) => unreachable!("pushed an Add"),
        }
    }

    /// Takes the port out of `slot`. It comes back through
    /// [`take_removed`](Self::take_removed) once the owner has let go.
    pub fn remove(&mut self, slot: usize) -> bool {
        if !self.is_used(slot) || self.changes.push(InputChange::Remove(slot)).is_err() {
            return false;
        }
        self.names[slot] = None;
        true
    }

    /// Ports the owner let go of since the last call.
    pub fn take_removed(&mut self) -> impl Iterator<Item = P> + '_ {
        self.removed.drain()
    }

    /// Full names of the ports in use, by slot.
    pub fn names(&self) -> impl Iterator<Item = (usize, &str)> {
        self.names
            .iter()
            .enumerate()
            .filter_map(|(i, n)| Some((i, n.as_deref()?)))
    }
}
