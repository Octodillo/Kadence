//! The host side of the CLAP API: callbacks the plugin makes into us.

use std::cell::{Cell, RefCell};
use std::ffi::CStr;
use std::os::fd::RawFd;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::ThreadId;
use std::time::{Duration, Instant};

use clack_extensions::audio_ports::{AudioPortRescanFlags, HostAudioPorts, HostAudioPortsImpl};
use clack_extensions::gui::{GuiSize, HostGui, HostGuiImpl};
use clack_extensions::latency::{HostLatency, HostLatencyImpl};
use clack_extensions::log::{HostLog, HostLogImpl, LogSeverity};
use clack_extensions::note_ports::{
    HostNotePorts, HostNotePortsImpl, NoteDialects, NotePortRescanFlags,
};
use clack_extensions::params::{
    HostParams, HostParamsImplMainThread, HostParamsImplShared, ParamClearFlags, ParamRescanFlags,
};
use clack_extensions::posix_fd::{FdFlags, HostPosixFd, HostPosixFdImpl};
use clack_extensions::state::{HostState, HostStateImpl};
use clack_extensions::thread_check::{HostThreadCheck, HostThreadCheckImpl};
use clack_extensions::timer::{HostTimer, HostTimerImpl, TimerId};
use clack_host::prelude::*;

thread_local! {
    /// Set while this thread is inside a plugin's `process` call.
    static IN_PROCESS: Cell<bool> = const { Cell::new(false) };
}

/// Marks the current thread as the audio thread until dropped. Scopes nest: dropping one
/// restores whatever was there before it.
pub(crate) struct AudioThreadScope {
    was: bool,
}

impl AudioThreadScope {
    pub(crate) fn enter() -> Self {
        Self {
            was: IN_PROCESS.with(|f| f.replace(true)),
        }
    }
}

impl Drop for AudioThreadScope {
    fn drop(&mut self) {
        IN_PROCESS.with(|f| f.set(self.was));
    }
}

pub(crate) struct DawHost;

impl HostHandlers for DawHost {
    type Shared<'a> = HostShared;
    type MainThread<'a> = HostMainThread<'a>;
    type AudioProcessor<'a> = ();

    fn declare_extensions(builder: &mut HostExtensions<Self>, _shared: &Self::Shared<'_>) {
        builder
            .register::<HostLog>()
            .register::<HostThreadCheck>()
            .register::<HostAudioPorts>()
            .register::<HostNotePorts>()
            .register::<HostParams>()
            .register::<HostLatency>()
            .register::<HostState>()
            .register::<HostGui>()
            .register::<HostTimer>()
            .register::<HostPosixFd>();
    }
}

pub(crate) fn new_instance(
    entry: &PluginEntry,
    id: &CStr,
) -> Result<PluginInstance<DawHost>, PluginInstanceError> {
    PluginInstance::<DawHost>::new(
        |_| HostShared::new(),
        |shared| HostMainThread::new(shared),
        entry,
        id,
        &crate::host_info(),
    )
}

/// Requests from the plugin, readable from any thread. The main-thread pump acts on them.
pub(crate) struct HostShared {
    main_thread: ThreadId,
    pub(crate) callback_requested: AtomicBool,
    pub(crate) restart_requested: AtomicBool,
    pub(crate) flush_requested: AtomicBool,
    pub(crate) gui_closed: AtomicBool,
    pub(crate) state_dirty: AtomicBool,
    /// An embedded editor's requested size, `width << 32 | height`, or 0 for none.
    pub(crate) pending_resize: AtomicU64,
}

impl HostShared {
    fn new() -> Self {
        Self {
            main_thread: std::thread::current().id(),
            callback_requested: AtomicBool::new(false),
            restart_requested: AtomicBool::new(false),
            flush_requested: AtomicBool::new(false),
            gui_closed: AtomicBool::new(false),
            state_dirty: AtomicBool::new(false),
            pending_resize: AtomicU64::new(0),
        }
    }

    pub(crate) fn take(flag: &AtomicBool) -> bool {
        flag.swap(false, Ordering::AcqRel)
    }
}

impl<'a> SharedHandler<'a> for HostShared {
    fn request_restart(&self) {
        self.restart_requested.store(true, Ordering::Release);
    }

    fn request_process(&self) {
        // The engine never puts plugins to sleep, so there is nothing to wake.
    }

    fn request_callback(&self) {
        self.callback_requested.store(true, Ordering::Release);
    }
}

impl HostLogImpl for HostShared {
    fn log(&self, severity: LogSeverity, message: &str) {
        // Printing is not real-time safe, so messages sent from the audio thread are dropped.
        if severity < LogSeverity::Warning || IN_PROCESS.with(Cell::get) {
            return;
        }
        eprintln!("[clap {severity}] {message}");
    }
}

impl HostThreadCheckImpl for HostShared {
    fn is_main_thread(&self) -> bool {
        std::thread::current().id() == self.main_thread
    }

    fn is_audio_thread(&self) -> bool {
        IN_PROCESS.with(Cell::get)
    }
}

impl HostParamsImplShared for HostShared {
    fn request_flush(&self) {
        self.flush_requested.store(true, Ordering::Release);
    }
}

impl HostGuiImpl for HostShared {
    fn resize_hints_changed(&self) {}

    fn request_resize(&self, new_size: GuiSize) -> Result<(), HostError> {
        // Applied to the host window by the main-thread pump; floating windows ignore it.
        let packed = (u64::from(new_size.width) << 32) | u64::from(new_size.height);
        self.pending_resize.store(packed, Ordering::Release);
        Ok(())
    }

    fn request_show(&self) -> Result<(), HostError> {
        Ok(())
    }

    fn request_hide(&self) -> Result<(), HostError> {
        Ok(())
    }

    fn closed(&self, _was_destroyed: bool) {
        self.gui_closed.store(true, Ordering::Release);
    }
}

pub(crate) struct Timer {
    pub(crate) id: TimerId,
    pub(crate) period: Duration,
    pub(crate) next: Instant,
}

/// Host state only touched on the main thread. clack does not check which thread a plugin calls
/// from, so the callbacks that touch the `RefCell`s check it themselves, and the flags are atomics.
pub(crate) struct HostMainThread<'a> {
    pub(crate) shared: &'a HostShared,
    pub(crate) latency_changed: AtomicBool,
    pub(crate) params_rescan: AtomicBool,
    pub(crate) timers: RefCell<Vec<Timer>>,
    next_timer_id: Cell<u32>,
    pub(crate) fds: RefCell<Vec<(RawFd, FdFlags)>>,
}

impl<'a> HostMainThread<'a> {
    fn new(shared: &'a HostShared) -> Self {
        Self {
            shared,
            latency_changed: AtomicBool::new(false),
            params_rescan: AtomicBool::new(false),
            timers: RefCell::new(Vec::new()),
            next_timer_id: Cell::new(0),
            fds: RefCell::new(Vec::new()),
        }
    }
}

impl HostMainThread<'_> {
    /// Fails for a plugin that breaks the CLAP rule that these callbacks come from the main
    /// thread, rather than racing the main thread for the `RefCell`s.
    fn check_main_thread(&self) -> Result<(), HostError> {
        if self.shared.is_main_thread() {
            Ok(())
        } else {
            Err(HostError::Message("called off the main thread"))
        }
    }
}

impl<'a> MainThreadHandler<'a> for HostMainThread<'a> {}

impl HostAudioPortsImpl for HostMainThread<'_> {
    fn is_rescan_flag_supported(&self, _flag: AudioPortRescanFlags) -> bool {
        false
    }

    fn rescan(&self, _flags: AudioPortRescanFlags) {
        // Port layouts are fixed between activations; a restart picks up any change.
        self.shared.restart_requested.store(true, Ordering::Release);
    }
}

impl HostNotePortsImpl for HostMainThread<'_> {
    fn supported_dialects(&self) -> NoteDialects {
        NoteDialects::CLAP | NoteDialects::MIDI
    }

    fn rescan(&self, _flags: NotePortRescanFlags) {
        self.shared.restart_requested.store(true, Ordering::Release);
    }
}

impl HostParamsImplMainThread for HostMainThread<'_> {
    fn rescan(&self, _flags: ParamRescanFlags) {
        self.params_rescan.store(true, Ordering::Release);
    }

    fn clear(&self, _param_id: ClapId, _flags: ParamClearFlags) {}
}

impl HostLatencyImpl for HostMainThread<'_> {
    fn changed(&self) {
        self.latency_changed.store(true, Ordering::Release);
    }
}

impl HostStateImpl for HostMainThread<'_> {
    fn mark_dirty(&self) {
        self.shared.state_dirty.store(true, Ordering::Release);
    }
}

impl HostTimerImpl for HostMainThread<'_> {
    fn register_timer(&self, period_ms: u32) -> Result<TimerId, HostError> {
        self.check_main_thread()?;
        let id = TimerId(self.next_timer_id.get());
        self.next_timer_id.set(id.0.wrapping_add(1));
        // CLAP allows the host to clamp periods; 15 ms keeps a 60 Hz pump meaningful.
        let period = Duration::from_millis(u64::from(period_ms.max(15)));
        self.timers.borrow_mut().push(Timer {
            id,
            period,
            next: Instant::now() + period,
        });
        Ok(id)
    }

    fn unregister_timer(&self, timer_id: TimerId) -> Result<(), HostError> {
        self.check_main_thread()?;
        let mut timers = self.timers.borrow_mut();
        let before = timers.len();
        timers.retain(|t| t.id != timer_id);
        if timers.len() == before {
            Err(HostError::Message("unknown timer"))
        } else {
            Ok(())
        }
    }
}

impl HostPosixFdImpl for HostMainThread<'_> {
    fn register_fd(&self, fd: RawFd, flags: FdFlags) -> Result<(), HostError> {
        self.check_main_thread()?;
        let mut fds = self.fds.borrow_mut();
        if fds.iter().any(|(f, _)| *f == fd) {
            return Err(HostError::Message("fd already registered"));
        }
        fds.push((fd, flags));
        Ok(())
    }

    fn modify_fd(&self, fd: RawFd, flags: FdFlags) -> Result<(), HostError> {
        self.check_main_thread()?;
        match self.fds.borrow_mut().iter_mut().find(|(f, _)| *f == fd) {
            Some(entry) => {
                entry.1 = flags;
                Ok(())
            }
            None => Err(HostError::Message("unknown fd")),
        }
    }

    fn unregister_fd(&self, fd: RawFd) -> Result<(), HostError> {
        self.check_main_thread()?;
        let mut fds = self.fds.borrow_mut();
        let before = fds.len();
        fds.retain(|(f, _)| *f != fd);
        if fds.len() == before {
            Err(HostError::Message("unknown fd"))
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lets a test call main-thread callbacks from another thread, as a misbehaving plugin can
    /// through the C API.
    struct AnyThread<'a>(&'a HostMainThread<'a>);
    // SAFETY: only used to check that the callbacks refuse to touch main-thread state.
    unsafe impl Send for AnyThread<'_> {}
    unsafe impl Sync for AnyThread<'_> {}

    impl<'a> AnyThread<'a> {
        fn get(&self) -> &'a HostMainThread<'a> {
            self.0
        }
    }

    #[test]
    fn main_thread_callbacks_refuse_other_threads() {
        let shared = HostShared::new();
        let host = HostMainThread::new(&shared);
        let any = AnyThread(&host);
        std::thread::scope(|s| {
            s.spawn(|| {
                let h = any.get();
                assert!(h.register_timer(20).is_err());
                assert!(h.register_fd(0, FdFlags::READ).is_err());
                HostParamsImplMainThread::rescan(h, ParamRescanFlags::VALUES);
                h.changed();
            });
        });
        assert!(host.timers.borrow().is_empty());
        assert!(host.fds.borrow().is_empty());
        // Flags are atomics, so a plugin that sets them from the wrong thread is still heard.
        assert!(host.params_rescan.swap(false, Ordering::AcqRel));
        assert!(host.latency_changed.swap(false, Ordering::AcqRel));
        assert!(host.register_timer(20).is_ok());
    }
}
