//! The `Linux::IRunLoop` plugins use for their own fds and timers (VSTGUI's X connection and
//! idle timers, for example). One run loop serves the whole process: plugins find it on the
//! host context and on each editor's `IPlugFrame`, and `on_main_thread` pumps it.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use vst3::ComPtr;
use vst3::Steinberg::Linux::{
    FileDescriptor, IEventHandler, IEventHandlerTrait, ITimerHandler, ITimerHandlerTrait,
    TimerInterval,
};
use vst3::Steinberg::{kInvalidArgument, kResultTrue, tresult};

struct Timer {
    handler: ComPtr<ITimerHandler>,
    interval: Duration,
    next: Instant,
}

#[derive(Default)]
pub struct RunLoop {
    fds: Mutex<Vec<(ComPtr<IEventHandler>, FileDescriptor)>>,
    timers: Mutex<Vec<Timer>>,
}

impl RunLoop {
    /// # Safety
    /// `handler` must be null or a valid `IEventHandler`.
    pub unsafe fn register_event_handler(
        &self,
        handler: *mut IEventHandler,
        fd: FileDescriptor,
    ) -> tresult {
        // `to_com_ptr` takes a reference of our own, released on unregister.
        let Some(handler) = (unsafe { vst3::ComRef::from_raw(handler) }) else {
            return kInvalidArgument;
        };
        self.fds.lock().unwrap().push((handler.to_com_ptr(), fd));
        kResultTrue
    }

    pub fn unregister_event_handler(&self, handler: *mut IEventHandler) -> tresult {
        self.fds
            .lock()
            .unwrap()
            .retain(|(h, _)| h.as_ptr() != handler);
        kResultTrue
    }

    /// # Safety
    /// `handler` must be null or a valid `ITimerHandler`.
    pub unsafe fn register_timer(
        &self,
        handler: *mut ITimerHandler,
        milliseconds: TimerInterval,
    ) -> tresult {
        let Some(handler) = (unsafe { vst3::ComRef::from_raw(handler) }) else {
            return kInvalidArgument;
        };
        let interval = Duration::from_millis(milliseconds.max(1));
        self.timers.lock().unwrap().push(Timer {
            handler: handler.to_com_ptr(),
            interval,
            next: Instant::now() + interval,
        });
        kResultTrue
    }

    pub fn unregister_timer(&self, handler: *mut ITimerHandler) -> tresult {
        self.timers
            .lock()
            .unwrap()
            .retain(|t| t.handler.as_ptr() != handler);
        kResultTrue
    }

    /// Runs ready fd handlers and due timers. Handlers may (un)register during the call, so the
    /// lists are copied first and no lock is held while plugin code runs.
    pub fn pump(&self) {
        let fds: Vec<_> = self.fds.lock().unwrap().clone();
        if !fds.is_empty() {
            let mut polls: Vec<libc::pollfd> = fds
                .iter()
                .map(|(_, fd)| libc::pollfd {
                    fd: *fd,
                    events: libc::POLLIN,
                    revents: 0,
                })
                .collect();
            let ready = unsafe { libc::poll(polls.as_mut_ptr(), polls.len() as libc::nfds_t, 0) };
            if ready > 0 {
                for ((handler, fd), p) in fds.iter().zip(&polls) {
                    if p.revents != 0 {
                        unsafe { handler.onFDIsSet(*fd) };
                    }
                }
            }
        }

        let now = Instant::now();
        let due: Vec<ComPtr<ITimerHandler>> = {
            let mut timers = self.timers.lock().unwrap();
            timers
                .iter_mut()
                .filter(|t| t.next <= now)
                .map(|t| {
                    t.next = now + t.interval;
                    t.handler.clone()
                })
                .collect()
        };
        for handler in due {
            unsafe { handler.onTimer() };
        }
    }
}

/// Implements `IRunLoopTrait` for a COM class by forwarding to a [`RunLoop`].
macro_rules! forward_run_loop {
    ($class:ty, $self:ident => $run_loop:expr) => {
        impl vst3::Steinberg::Linux::IRunLoopTrait for $class {
            unsafe fn registerEventHandler(
                &$self,
                handler: *mut vst3::Steinberg::Linux::IEventHandler,
                fd: vst3::Steinberg::Linux::FileDescriptor,
            ) -> vst3::Steinberg::tresult {
                unsafe { $run_loop.register_event_handler(handler, fd) }
            }

            unsafe fn unregisterEventHandler(
                &$self,
                handler: *mut vst3::Steinberg::Linux::IEventHandler,
            ) -> vst3::Steinberg::tresult {
                $run_loop.unregister_event_handler(handler)
            }

            unsafe fn registerTimer(
                &$self,
                handler: *mut vst3::Steinberg::Linux::ITimerHandler,
                milliseconds: vst3::Steinberg::Linux::TimerInterval,
            ) -> vst3::Steinberg::tresult {
                unsafe { $run_loop.register_timer(handler, milliseconds) }
            }

            unsafe fn unregisterTimer(
                &$self,
                handler: *mut vst3::Steinberg::Linux::ITimerHandler,
            ) -> vst3::Steinberg::tresult {
                $run_loop.unregister_timer(handler)
            }
        }
    };
}

pub(crate) use forward_run_loop;
