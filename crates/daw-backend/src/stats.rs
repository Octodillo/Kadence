use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering::Relaxed};

/// A snapshot of a backend's counters. See [`crate::AudioBackend::stats`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BackendStats {
    /// Process cycles run since the backend was opened.
    pub cycles: u64,
    /// Frames processed since the backend was opened.
    pub frames: u64,
    /// Buffer under- or overruns reported by the audio system (or, for the
    /// dummy backend in real-time pacing, cycles that missed their deadline).
    pub xruns: u64,
    /// MIDI events dropped because an event list was full.
    pub midi_dropped: u64,
    /// Whether the process thread runs with a real-time scheduling policy
    /// (`SCHED_FIFO` or `SCHED_RR`). `None` until the first cycle has run.
    /// JACK re-checks while it reads `false`, because PipeWire grants
    /// real-time asynchronously.
    pub realtime: Option<bool>,
    /// Time spent processing as a percentage of the period, when known.
    pub dsp_load: Option<f32>,
    /// The audio server shut down or kicked us out. The backend is dead and
    /// must be closed and reopened.
    pub server_gone: bool,
    /// The process callback panicked. The backend keeps running but no
    /// longer calls it, and outputs silence until the next
    /// [`start`](crate::AudioBackend::start). (JACK only; the dummy backend's
    /// `stop` returns an error instead.)
    pub callback_panicked: bool,
    /// Allocations caught inside cycles (debug builds with
    /// `assert_no_alloc::AllocDisabler` installed; always 0 otherwise).
    pub rt_alloc_violations: u64,
}

/// Counters written by the real-time thread and read by everyone else.
#[derive(Default)]
pub(crate) struct SharedStats {
    pub cycles: AtomicU64,
    pub frames: AtomicU64,
    pub xruns: AtomicU64,
    pub midi_dropped: AtomicU64,
    /// 0 = unknown, 1 = not real-time, 2 = real-time.
    realtime: AtomicU8,
    /// f32 bits; NaN = unknown.
    dsp_load: AtomicU32,
    pub server_gone: AtomicBool,
    pub callback_panicked: AtomicBool,
    pub rt_alloc_violations: AtomicU64,
    pub sample_rate: AtomicU32,
    pub buffer_size: AtomicU32,
}

impl SharedStats {
    pub fn new() -> Self {
        let s = Self::default();
        s.dsp_load.store(f32::NAN.to_bits(), Relaxed);
        s
    }

    pub fn set_realtime(&self, rt: bool) {
        self.realtime.store(if rt { 2 } else { 1 }, Relaxed);
    }

    pub fn set_dsp_load(&self, percent: f32) {
        self.dsp_load.store(percent.to_bits(), Relaxed);
    }

    pub fn snapshot(&self) -> BackendStats {
        let load = f32::from_bits(self.dsp_load.load(Relaxed));
        BackendStats {
            cycles: self.cycles.load(Relaxed),
            frames: self.frames.load(Relaxed),
            xruns: self.xruns.load(Relaxed),
            midi_dropped: self.midi_dropped.load(Relaxed),
            realtime: match self.realtime.load(Relaxed) {
                0 => None,
                v => Some(v == 2),
            },
            dsp_load: (!load.is_nan()).then_some(load),
            server_gone: self.server_gone.load(Relaxed),
            callback_panicked: self.callback_panicked.load(Relaxed),
            rt_alloc_violations: self.rt_alloc_violations.load(Relaxed),
        }
    }
}

/// Whether the calling thread has a real-time scheduling policy. Does not
/// allocate or lock, so it is safe on the process thread.
pub(crate) fn current_thread_is_realtime() -> bool {
    // Ask the kernel, not `pthread_getschedparam`: glibc caches the policy on
    // its first call and never sees rtkit or the portal promoting the thread
    // from outside. One syscall.
    // SAFETY: plain libc query on the current thread.
    let policy = unsafe { libc::sched_getscheduler(0) };
    // Mask SCHED_RESET_ON_FORK, which rtkit sets alongside the policy.
    let policy = policy & !libc::SCHED_RESET_ON_FORK;
    policy == libc::SCHED_FIFO || policy == libc::SCHED_RR
}

#[cfg(test)]
mod tests {
    use super::*;

    /// rtkit and the portal promote the thread from outside, by TID, after
    /// the first check. The check must see that, so it cannot rely on a
    /// value glibc cached on the first call.
    #[test]
    fn sees_policy_changed_by_tid_after_first_check() {
        std::thread::spawn(|| {
            assert!(!current_thread_is_realtime());
            // SAFETY: plain libc calls on the current thread.
            let promoted = unsafe {
                let param = libc::sched_param { sched_priority: 1 };
                libc::sched_setscheduler(libc::gettid(), libc::SCHED_FIFO, &param) == 0
            };
            if !promoted {
                eprintln!("skipped: no permission to set SCHED_FIFO");
                return;
            }
            assert!(current_thread_is_realtime());
        })
        .join()
        .unwrap();
    }
}
