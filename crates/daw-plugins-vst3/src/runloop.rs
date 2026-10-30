//! The `Linux::IRunLoop` plugins use for their own fds and timers (VSTGUI's X connection and
//! idle timers, for example). One run loop serves the whole process: plugins find it on the
//! host context and on each editor's `IPlugFrame`, and `on_main_thread` pumps it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vst3::ComPtr;
use vst3::Steinberg::Linux::{
    FileDescriptor, IEventHandler, IEventHandlerTrait, ITimerHandler, ITimerHandlerTrait,
    TimerInterval,
};
use vst3::Steinberg::{kInvalidArgument, kResultTrue, tresult};

/// One registered handler. `live` turns false on unregister, so a pump that copied the
/// registration before then skips it.
struct Registration<H: vst3::Interface> {
    handler: ComPtr<H>,
    live: AtomicBool,
}

impl<H: vst3::Interface> Registration<H> {
    fn new(handler: ComPtr<H>) -> Arc<Self> {
        Arc::new(Registration {
            handler,
            live: AtomicBool::new(true),
        })
    }

    fn is(&self, handler: *mut H) -> bool {
        self.handler.as_ptr() == handler
    }

    fn live(&self) -> bool {
        self.live.load(Ordering::Acquire)
    }
}

struct Fd {
    registration: Arc<Registration<IEventHandler>>,
    fd: FileDescriptor,
}

struct Timer {
    registration: Arc<Registration<ITimerHandler>>,
    interval: Duration,
    next: Instant,
}

#[derive(Default)]
pub struct RunLoop {
    fds: Mutex<Vec<Fd>>,
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
        self.fds.lock().unwrap().push(Fd {
            registration: Registration::new(handler.to_com_ptr()),
            fd,
        });
        kResultTrue
    }

    pub fn unregister_event_handler(&self, handler: *mut IEventHandler) -> tresult {
        self.fds.lock().unwrap().retain(|f| {
            let keep = !f.registration.is(handler);
            if !keep {
                f.registration.live.store(false, Ordering::Release);
            }
            keep
        });
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
            registration: Registration::new(handler.to_com_ptr()),
            interval,
            next: Instant::now() + interval,
        });
        kResultTrue
    }

    pub fn unregister_timer(&self, handler: *mut ITimerHandler) -> tresult {
        self.timers.lock().unwrap().retain(|t| {
            let keep = !t.registration.is(handler);
            if !keep {
                t.registration.live.store(false, Ordering::Release);
            }
            keep
        });
        kResultTrue
    }

    /// Runs ready fd handlers and due timers. Handlers may (un)register during the call, so the
    /// lists are copied first and no lock is held while plugin code runs. A handler
    /// unregistered by an earlier one in the same pump (an editor closed from a timer, say)
    /// is skipped: its object may be torn down and its fd closed or reused.
    pub fn pump(&self) {
        let fds: Vec<_> = self
            .fds
            .lock()
            .unwrap()
            .iter()
            .map(|f| (f.registration.clone(), f.fd))
            .collect();
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
                for ((registration, fd), p) in fds.iter().zip(&polls) {
                    if p.revents != 0 && registration.live() {
                        unsafe { registration.handler.onFDIsSet(*fd) };
                    }
                }
            }
        }

        let now = Instant::now();
        let due: Vec<_> = {
            let mut timers = self.timers.lock().unwrap();
            timers
                .iter_mut()
                .filter(|t| t.next <= now)
                .map(|t| {
                    t.next = now + t.interval;
                    t.registration.clone()
                })
                .collect()
        };
        for registration in due {
            if registration.live() {
                unsafe { registration.handler.onTimer() };
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU32;
    use vst3::{Class, ComWrapper};

    /// A timer or fd handler that counts its calls and, on its first call, unregisters
    /// another handler, as closing one editor from another's timer does.
    struct Closer {
        run_loop: Arc<RunLoop>,
        victim_timer: Mutex<Option<*mut ITimerHandler>>,
        victim_fd: Mutex<Option<*mut IEventHandler>>,
        calls: AtomicU32,
    }

    impl Closer {
        fn new(run_loop: &Arc<RunLoop>) -> ComWrapper<Closer> {
            ComWrapper::new(Closer {
                run_loop: run_loop.clone(),
                victim_timer: Mutex::new(None),
                victim_fd: Mutex::new(None),
                calls: AtomicU32::new(0),
            })
        }

        fn fire(&self) {
            self.calls.fetch_add(1, Ordering::Relaxed);
            if let Some(victim) = self.victim_timer.lock().unwrap().take() {
                self.run_loop.unregister_timer(victim);
            }
            if let Some(victim) = self.victim_fd.lock().unwrap().take() {
                self.run_loop.unregister_event_handler(victim);
            }
        }

        fn calls(&self) -> u32 {
            self.calls.load(Ordering::Relaxed)
        }
    }

    impl Class for Closer {
        type Interfaces = (ITimerHandler, IEventHandler);
    }

    impl ITimerHandlerTrait for Closer {
        unsafe fn onTimer(&self) {
            self.fire();
        }
    }

    impl IEventHandlerTrait for Closer {
        unsafe fn onFDIsSet(&self, _fd: FileDescriptor) {
            self.fire();
        }
    }

    #[test]
    fn a_timer_unregistered_earlier_in_the_same_pump_is_not_called() {
        let run_loop = Arc::new(RunLoop::default());
        let (a, b) = (Closer::new(&run_loop), Closer::new(&run_loop));
        let ptr = |c: &ComWrapper<Closer>| c.as_com_ref::<ITimerHandler>().unwrap().as_ptr();
        // Each one closes the other, so whichever runs first must stop the second.
        *a.victim_timer.lock().unwrap() = Some(ptr(&b));
        *b.victim_timer.lock().unwrap() = Some(ptr(&a));
        unsafe {
            run_loop.register_timer(ptr(&a), 1);
            run_loop.register_timer(ptr(&b), 1);
        }
        std::thread::sleep(Duration::from_millis(5));
        run_loop.pump();
        assert_eq!(a.calls() + b.calls(), 1, "an unregistered timer still ran");
    }

    #[test]
    fn an_fd_handler_unregistered_earlier_in_the_same_pump_is_not_called() {
        let mut pipe = [0; 2];
        assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0);
        assert_eq!(unsafe { libc::write(pipe[1], b"x".as_ptr().cast(), 1) }, 1);

        let run_loop = Arc::new(RunLoop::default());
        let (a, b) = (Closer::new(&run_loop), Closer::new(&run_loop));
        let ptr = |c: &ComWrapper<Closer>| c.as_com_ref::<IEventHandler>().unwrap().as_ptr();
        *a.victim_fd.lock().unwrap() = Some(ptr(&b));
        *b.victim_fd.lock().unwrap() = Some(ptr(&a));
        unsafe {
            run_loop.register_event_handler(ptr(&a), pipe[0]);
            run_loop.register_event_handler(ptr(&b), pipe[0]);
        }
        run_loop.pump();
        assert_eq!(
            a.calls() + b.calls(),
            1,
            "an unregistered fd handler still ran"
        );
        unsafe {
            libc::close(pipe[0]);
            libc::close(pipe[1]);
        }
    }
}
