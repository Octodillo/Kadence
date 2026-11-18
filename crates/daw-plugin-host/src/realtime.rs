//! Real-time scheduling for the host's plugin-audio thread.
//!
//! The application's audio thread waits for that thread in every cycle, so it has to be
//! scheduled like one. [`promote`] first asks the kernel directly, which works where the user
//! may raise priorities themselves (`RLIMIT_RTPRIO`, as the `realtime` group gives it). On a
//! stock desktop that is refused, and a broker on D-Bus grants it instead: RealtimeKit on the
//! system bus, or the desktop portal's Realtime interface on the session bus, which is the one
//! that can be reached from inside a container or a Flatpak. PipeWire takes the same three
//! routes for the application's own audio thread.
//!
//! Asking a broker means D-Bus round trips, and a bus that does not answer must not hold up
//! the host. So the host hands each audio thread it starts to a [`Promoter`], whose own
//! thread does the asking and leaves the outcome, a [`Realtime`], in the block's header for
//! the application to show.
//!
//! RealtimeKit rations what it grants: by default 15 processes and 25 threads per user, and
//! 25 requests in 20 seconds, and the portal passes requests on to it. Every plugin has its
//! own host process, so a large project can use the ration up and its later plugins are
//! refused. A refusal is asked about again a few times, which gets a project loaded in one
//! go past the limit on requests; nothing here gets past the limit on processes. Granting
//! `RLIMIT_RTPRIO` to the user (the `realtime` group) lifts both.
//!
//! A broker also demands that the process bounds how long its real-time threads may run
//! without sleeping (`RLIMIT_RTTIME`, 200 ms with RealtimeKit's defaults). The kernel kills a
//! process that goes over, so a plugin that computes for that long inside one cycle takes its
//! host down, which the application reports as a crash.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::Duration;

use zbus::blocking::Connection;
use zbus::blocking::connection::Builder as ConnectionBuilder;
use zbus::blocking::proxy::Builder as ProxyBuilder;
use zbus::proxy::CacheProperties;

use crate::shm::Shm;

/// How a host's plugin-audio thread is scheduled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u32)]
pub enum Realtime {
    /// Not asked for ([`SandboxConfig::rt_priority`](crate::SandboxConfig) is `None`), or the
    /// host has no audio thread yet.
    #[default]
    Off = 0,
    /// The kernel granted it to the host itself.
    Direct = 1,
    /// RealtimeKit or the desktop portal granted it.
    Brokered = 2,
    /// Nobody granted it: the thread runs at normal priority and the plugin's audio will
    /// drop out whenever the machine is busy.
    Refused = 3,
    /// Asked for, and no answer yet. A bus that never answers leaves it here.
    Pending = 4,
}

impl Realtime {
    pub(crate) fn from_wire(word: u32) -> Self {
        match word {
            1 => Self::Direct,
            2 => Self::Brokered,
            3 => Self::Refused,
            4 => Self::Pending,
            _ => Self::Off,
        }
    }

    /// Whether the thread has real-time scheduling.
    pub fn is_granted(self) -> bool {
        matches!(self, Self::Direct | Self::Brokered)
    }
}

/// How long a broker may take to answer a call before the next route is tried.
const BROKER_TIMEOUT: Duration = Duration::from_secs(2);
/// How long after a refusal the routes are tried again: the time in which RealtimeKit counts
/// requests against its limit.
const RETRY_AFTER: Duration = Duration::from_secs(20);
/// How often a refused thread is asked about again.
const RETRIES: u32 = 3;

/// A D-Bus service that makes threads real-time for processes that may not do it themselves.
/// RealtimeKit and the portal offer the same properties and method under different names.
struct Broker {
    name: &'static str,
    bus: fn() -> zbus::Result<ConnectionBuilder<'static>>,
    destination: &'static str,
    path: &'static str,
    interface: &'static str,
}

const BROKERS: [Broker; 2] = [
    Broker {
        name: "RealtimeKit",
        bus: ConnectionBuilder::system,
        destination: "org.freedesktop.RealtimeKit1",
        path: "/org/freedesktop/RealtimeKit1",
        interface: "org.freedesktop.RealtimeKit1",
    },
    Broker {
        name: "the desktop portal",
        bus: ConnectionBuilder::session,
        destination: "org.freedesktop.portal.Desktop",
        path: "/org/freedesktop/portal/desktop",
        interface: "org.freedesktop.portal.Realtime",
    },
];

/// Why a route did not give the thread real-time scheduling.
#[derive(Debug)]
enum Refusal {
    Os(std::io::Error),
    Bus(zbus::Error),
    /// The broker allows no real-time priority at all.
    NoPriority,
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Os(e) => e.fmt(f),
            Self::Bus(e) => e.fmt(f),
            Self::NoPriority => f.write_str("it allows no real-time priority"),
        }
    }
}

impl From<zbus::Error> for Refusal {
    fn from(e: zbus::Error) -> Self {
        Self::Bus(e)
    }
}

impl Broker {
    fn promote(&self, thread: libc::pid_t, priority: i32) -> Result<(), Refusal> {
        let bus = (self.bus)()?.method_timeout(BROKER_TIMEOUT).build()?;
        self.promote_on(&bus, thread, priority)
    }

    fn promote_on(
        &self,
        bus: &Connection,
        thread: libc::pid_t,
        priority: i32,
    ) -> Result<(), Refusal> {
        let proxy: zbus::blocking::Proxy<'_> = ProxyBuilder::new(bus)
            .destination(self.destination)?
            .path(self.path)?
            .interface(self.interface)?
            .cache_properties(CacheProperties::No)
            .build()?;
        let max_priority: i32 = proxy.get_property("MaxRealtimePriority")?;
        let max_run: i64 = proxy.get_property("RTTimeUSecMax")?;
        let priority = u32::try_from(priority.min(max_priority))
            .ok()
            .filter(|p| *p > 0)
            .ok_or(Refusal::NoPriority)?;
        // A broker only promotes threads of a process that has bounded how long they may run
        // without sleeping, so a runaway one cannot take the machine with it.
        limit_realtime_run(max_run).map_err(Refusal::Os)?;
        let process = u64::from(std::process::id());
        let thread = u64::try_from(thread).unwrap_or_default();
        proxy.call::<_, _, ()>("MakeThreadRealtimeWithPID", &(process, thread, priority))?;
        Ok(())
    }
}

/// Lowers `RLIMIT_RTTIME` to `micros` where it is higher: the time a real-time thread of this
/// process may run without a blocking call before the kernel kills the process. Brokers look
/// at the hard limit, and both are set, as PipeWire does. See the module documentation for
/// what that means for plugins.
fn limit_realtime_run(micros: i64) -> std::io::Result<()> {
    let Ok(micros) = libc::rlim_t::try_from(micros) else {
        return Ok(());
    };
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: plain syscall wrappers given a valid struct to fill and to read.
    unsafe {
        if libc::getrlimit(libc::RLIMIT_RTTIME, &mut limit) != 0 {
            return Err(std::io::Error::last_os_error());
        }
        if limit.rlim_cur <= micros && limit.rlim_max <= micros {
            return Ok(());
        }
        limit.rlim_cur = limit.rlim_cur.min(micros);
        limit.rlim_max = limit.rlim_max.min(micros);
        if libc::setrlimit(libc::RLIMIT_RTTIME, &limit) != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Asks the kernel for `SCHED_FIFO` for the thread, at `priority` or the highest priority
/// this process may give itself. Threads the plugin starts from it do not inherit it, as with
/// a broker.
fn promote_directly(thread: libc::pid_t, priority: i32) -> std::io::Result<()> {
    let param = libc::sched_param {
        sched_priority: priority.min(own_priority_limit().unwrap_or(priority)),
    };
    let policy = libc::SCHED_FIFO | libc::SCHED_RESET_ON_FORK;
    // SAFETY: plain syscall wrapper given a valid parameter struct.
    match unsafe { libc::sched_setscheduler(thread, policy, &param) } {
        0 => Ok(()),
        _ => Err(std::io::Error::last_os_error()),
    }
}

/// The highest real-time priority `RLIMIT_RTPRIO` lets this process take, if it lets it take
/// any. Without one, only a privileged process gets what it asks for.
fn own_priority_limit() -> Option<i32> {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: fills the struct it is given.
    let read = unsafe { libc::getrlimit(libc::RLIMIT_RTPRIO, &mut limit) } == 0;
    (read && limit.rlim_cur > 0)
        .then(|| i32::try_from(limit.rlim_cur).ok())
        .flatten()
}

/// The calling thread's id, as [`promote`] takes it.
pub(crate) fn current_thread() -> libc::pid_t {
    // SAFETY: gettid takes no arguments and cannot fail. The raw call, since the C library's
    // wrapper is younger than some systems this runs on.
    let id = unsafe { libc::syscall(libc::SYS_gettid) };
    libc::pid_t::try_from(id).expect("a thread id fits pid_t")
}

/// Gives a thread of this process real-time scheduling at `priority`, or the highest
/// priority below it that is allowed: `SCHED_FIFO` from the kernel, or whatever policy a
/// broker gives (RealtimeKit's default is `SCHED_RR`). It may take as long as the brokers
/// take to answer. With `explain`, a refusal is logged with each route's reason.
fn promote(thread: libc::pid_t, priority: i32, explain: bool) -> Realtime {
    let direct = match promote_directly(thread, priority) {
        Ok(()) => return Realtime::Direct,
        // The thread is gone: its host was prepared again, and the new one is asked for.
        Err(e) if e.raw_os_error() == Some(libc::ESRCH) => return Realtime::Off,
        Err(e) => e,
    };
    let mut reasons = format!("the kernel: {direct}");
    for broker in &BROKERS {
        match broker.promote(thread, priority) {
            Ok(()) => return Realtime::Brokered,
            Err(e) => reasons.push_str(&format!("; {}: {e}", broker.name)),
        }
    }
    if explain {
        log::warn!("real-time priority {priority} not granted, audio may glitch ({reasons})");
    }
    Realtime::Refused
}

/// An audio thread to promote, and the block whose header gets the outcome.
struct Request {
    thread: libc::pid_t,
    priority: i32,
    block: Arc<Shm>,
    /// How often it has been refused.
    refusals: u32,
}

/// Promotes the host's audio threads on a thread of its own, started with the first request.
#[derive(Default)]
pub(crate) struct Promoter {
    requests: Option<Sender<Request>>,
}

impl Promoter {
    /// Asks for real-time scheduling at `priority` for `thread`, which serves `block`. The
    /// block's header says [`Realtime::Pending`] until the outcome is known.
    pub(crate) fn ask(&mut self, thread: libc::pid_t, priority: i32, block: Arc<Shm>) {
        let header = block.header();
        header
            .realtime
            .store(Realtime::Pending as u32, Ordering::Release);
        let request = Request {
            thread,
            priority,
            block,
            refusals: 0,
        };
        let requests = self.requests.get_or_insert_with(|| {
            let (requests, inbox) = mpsc::channel();
            // Without the thread the requests stay pending, which is what they then are.
            let _ = std::thread::Builder::new()
                .name("plugin-realtime".into())
                .spawn(move || serve(&inbox));
            requests
        });
        let _ = requests.send(request);
    }
}

/// Answers requests until the [`Promoter`] is dropped. Only the newest one counts: an older
/// one is for an audio thread that has ended since.
fn serve(inbox: &Receiver<Request>) {
    let mut refused: Option<Request> = None;
    loop {
        let next = match &refused {
            None => inbox.recv().map_err(|_| RecvTimeoutError::Disconnected),
            Some(_) => inbox.recv_timeout(RETRY_AFTER),
        };
        let request = match next {
            Ok(request) => inbox.try_iter().last().unwrap_or(request),
            Err(RecvTimeoutError::Timeout) => refused.take().expect("it timed out waiting to"),
            Err(RecvTimeoutError::Disconnected) => return,
        };
        let outcome = promote(request.thread, request.priority, request.refusals == 0);
        request
            .block
            .header()
            .realtime
            .store(outcome as u32, Ordering::Release);
        refused = (outcome == Realtime::Refused && request.refusals < RETRIES).then(|| Request {
            refusals: request.refusals + 1,
            ..request
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixStream;
    use std::sync::{Arc, Mutex};

    /// What RealtimeKit looks like to its callers.
    struct FakeKit {
        max_priority: i32,
        max_run: i64,
        refuse: bool,
        asked: Arc<Mutex<Vec<(u64, u64, u32)>>>,
    }

    #[zbus::interface(name = "org.freedesktop.RealtimeKit1")]
    impl FakeKit {
        #[zbus(property)]
        fn max_realtime_priority(&self) -> i32 {
            self.max_priority
        }

        #[zbus(property, name = "RTTimeUSecMax")]
        fn rt_time_usec_max(&self) -> i64 {
            self.max_run
        }

        #[zbus(name = "MakeThreadRealtimeWithPID")]
        fn make_thread_realtime(
            &self,
            process: u64,
            thread: u64,
            priority: u32,
        ) -> zbus::fdo::Result<()> {
            self.asked.lock().unwrap().push((process, thread, priority));
            if self.refuse {
                return Err(zbus::fdo::Error::AccessDenied("not for you".into()));
            }
            Ok(())
        }
    }

    /// Serves `kit` on one end of a socket pair and returns a connection to it, with the
    /// serving connection to keep alive.
    fn serve(kit: FakeKit) -> (Connection, Connection) {
        let (ours, theirs) = UnixStream::pair().unwrap();
        let server = std::thread::spawn(move || {
            ConnectionBuilder::async_io_unix_stream(theirs)
                .server(zbus::Guid::generate())
                .unwrap()
                .p2p()
                .serve_at(BROKERS[0].path, kit)
                .unwrap()
                .build()
                .unwrap()
        });
        let client = ConnectionBuilder::async_io_unix_stream(ours)
            .p2p()
            .method_timeout(BROKER_TIMEOUT)
            .build()
            .unwrap();
        (client, server.join().unwrap())
    }

    fn realtime_run_limit() -> libc::rlimit {
        let mut limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: fills the struct it is given.
        assert_eq!(
            unsafe { libc::getrlimit(libc::RLIMIT_RTTIME, &mut limit) },
            0
        );
        limit
    }

    #[test]
    fn a_broker_is_asked_within_what_it_allows() {
        let asked = Arc::new(Mutex::new(Vec::new()));
        let (bus, _server) = serve(FakeKit {
            max_priority: 20,
            max_run: 200_000,
            refuse: false,
            asked: asked.clone(),
        });
        let thread = current_thread();
        BROKERS[0].promote_on(&bus, thread, 70).unwrap();
        // The priority is cut to the broker's maximum, and the thread is named by process.
        assert_eq!(
            *asked.lock().unwrap(),
            [(u64::from(std::process::id()), thread as u64, 20)]
        );
        // The process has bounded its real-time threads as the broker demands.
        let limit = realtime_run_limit();
        assert!(limit.rlim_cur <= 200_000 && limit.rlim_max <= 200_000);
    }

    #[test]
    fn a_brokers_refusal_is_a_refusal() {
        let asked = Arc::new(Mutex::new(Vec::new()));
        let (bus, _server) = serve(FakeKit {
            max_priority: 20,
            max_run: 200_000,
            refuse: true,
            asked: asked.clone(),
        });
        let refusal = BROKERS[0]
            .promote_on(&bus, current_thread(), 70)
            .unwrap_err();
        assert!(matches!(refusal, Refusal::Bus(_)), "{refusal}");
        assert_eq!(asked.lock().unwrap().len(), 1);
    }

    #[test]
    fn a_broker_that_allows_no_priority_is_not_asked() {
        let asked = Arc::new(Mutex::new(Vec::new()));
        let (bus, _server) = serve(FakeKit {
            max_priority: 0,
            max_run: 200_000,
            refuse: false,
            asked: asked.clone(),
        });
        let refusal = BROKERS[0]
            .promote_on(&bus, current_thread(), 70)
            .unwrap_err();
        assert!(matches!(refusal, Refusal::NoPriority), "{refusal}");
        assert!(asked.lock().unwrap().is_empty());
    }

    #[test]
    fn the_wire_word_round_trips() {
        for r in [
            Realtime::Off,
            Realtime::Direct,
            Realtime::Brokered,
            Realtime::Refused,
            Realtime::Pending,
        ] {
            assert_eq!(Realtime::from_wire(r as u32), r);
        }
    }
}
