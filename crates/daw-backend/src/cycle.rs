use daw_core::event::EventList;

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
