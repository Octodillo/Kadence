//! The application's end of the control socket to one host process.
//!
//! Requests and replies alternate, one at a time, and both sides number the requests in the
//! order they are sent. The host's main thread reads them between its pumps of the plugin's
//! main-thread callbacks, so a plugin that stays in one (its editor showing a modal file
//! dialog, say) cannot answer until the user is done. Such a host is busy, not hung: when a
//! call finds it there with the editor open, the call gives up with [`CallError::Busy`] and
//! leaves the host running. It withdraws the request through the audio block's header, so the
//! host answers it without running it, unless the request [can wait](Request::can_wait), which
//! the host then runs once it is free. Either way the reply still comes, and is dropped before
//! the next one is read.

use std::cell::{Cell, RefCell};
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use crate::protocol::{FrameReader, Request, Response, write_frame};
use crate::shm::Header;

/// How long a call waits on a host whose main thread is in one pump with the plugin's editor
/// open before it calls the host busy. Pumps of a plugin that is not waiting on the user take
/// a few milliseconds.
pub(crate) const EDITOR_BUSY_AFTER: Duration = Duration::from_millis(250);

/// How often a call that watches for a busy host looks at the header while it waits.
const WATCH_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Debug)]
pub(crate) enum CallError {
    /// The host's main thread is busy in the plugin's open editor. The request was withdrawn
    /// or left to run later, and the host is fine.
    Busy,
    /// The host broke the protocol, went away or did not answer in time.
    Io(io::Error),
}

impl From<io::Error> for CallError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Busy => f.write_str("the plugin is busy in its own window, such as a dialog"),
            Self::Io(e) => e.fmt(f),
        }
    }
}

/// The application's end of one host's control socket.
pub(crate) struct Control {
    stream: UnixStream,
    /// Keeps bytes of a reply across calls, since a call can give up partway through one.
    reader: RefCell<FrameReader>,
    /// Bytes of requests not written yet: a call that gave up on a busy host can leave part
    /// of its request behind, which the next call writes first.
    unsent: RefCell<Vec<u8>>,
    /// Requests started so far, the last one's number.
    sent: Cell<u64>,
    /// Replies still to come for requests given up on, to drop before the next.
    owed: Cell<u64>,
    /// The pump a call last gave up in. Calls fail at once while the host is still in it.
    busy_in: Cell<Option<u32>>,
}

impl Control {
    pub(crate) fn new(stream: UnixStream) -> Self {
        Self {
            stream,
            reader: RefCell::default(),
            unsent: RefCell::default(),
            sent: Cell::new(0),
            owed: Cell::new(0),
            busy_in: Cell::new(None),
        }
    }

    /// Sends `request` and returns its reply, all within `timeout`, however the host paces its
    /// bytes. With `header`, the block the host uses now, a host found busy in the plugin's
    /// editor makes the call return [`CallError::Busy`] instead of waiting it out.
    pub(crate) fn call(
        &self,
        request: &Request,
        blob: &[u8],
        timeout: Duration,
        header: Option<&Header>,
    ) -> Result<(Response, Vec<u8>), CallError> {
        let mut watch = Watch { header, seen: None };
        if let (Some(busy), Some(h)) = (self.busy_in.get(), header) {
            if h.pumping_editor() == Some(busy) {
                return Err(CallError::Busy);
            }
            self.busy_in.set(None);
        }
        let deadline = Instant::now() + timeout.max(Duration::from_millis(1));
        // What an earlier call left unwritten goes first. Until it is out, this request is not
        // sent at all.
        self.flush(&mut watch, deadline)?;

        let n = self.sent.get() + 1;
        self.sent.set(n);
        write_frame(&mut *self.unsent.borrow_mut(), request, blob)?;
        if let Err(e) = self.flush(&mut watch, deadline) {
            if matches!(e, CallError::Busy) && !self.give_up(n, request, &watch) {
                // The host took the request after all; we are stuck with it mid-frame.
                return Err(CallError::Io(timed_out()));
            }
            return Err(e);
        }

        loop {
            if let Some(frame) = self.read_frame(&watch, deadline)? {
                if self.owed.get() > 0 {
                    self.owed.set(self.owed.get() - 1);
                    continue;
                }
                return Ok(frame);
            }
            if watch.busy() && self.give_up(n, request, &watch) {
                return Err(CallError::Busy);
            }
            if Instant::now() >= deadline {
                return Err(CallError::Io(timed_out()));
            }
        }
    }

    /// Stops waiting for request `n` (the latest) on a busy host, if the host has not started
    /// it. Returns false if it has: then its reply is on its way.
    fn give_up(&self, n: u64, request: &Request, watch: &Watch<'_>) -> bool {
        let Some(h) = watch.header else {
            return false;
        };
        let given_up = if request.can_wait() {
            !h.request_started(n)
        } else {
            h.withdraw_request(n)
        };
        if given_up {
            self.owed.set(self.owed.get() + 1);
            self.busy_in.set(h.pumping_editor());
        }
        given_up
    }

    /// Writes what is unsent. Fails with [`CallError::Busy`] if the host stops reading because
    /// it is busy, leaving the rest unsent.
    fn flush(&self, watch: &mut Watch<'_>, deadline: Instant) -> Result<(), CallError> {
        let mut unsent = self.unsent.borrow_mut();
        let mut stream = self.timed(watch, deadline);
        let mut written = 0;
        let result = loop {
            if written == unsent.len() {
                break Ok(());
            }
            match stream.write(&unsent[written..]) {
                Ok(0) => break Err(io::Error::from(io::ErrorKind::WriteZero).into()),
                Ok(k) => written += k,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) if is_timeout(&e) && Instant::now() < deadline => {
                    if watch.busy() {
                        break Err(CallError::Busy);
                    }
                }
                Err(e) => break Err(e.into()),
            }
        };
        unsent.drain(..written);
        result
    }

    /// Reads for at most one watch interval (or until `deadline`, without a header), and
    /// returns a complete frame if there is one.
    fn read_frame(
        &self,
        watch: &Watch<'_>,
        deadline: Instant,
    ) -> io::Result<Option<(Response, Vec<u8>)>> {
        self.reader.borrow_mut().poll(self.timed(watch, deadline))
    }

    fn timed(&self, watch: &Watch<'_>, deadline: Instant) -> Timed<'_> {
        Timed {
            stream: &self.stream,
            deadline,
            slice: watch.header.map(|_| WATCH_INTERVAL),
        }
    }
}

/// The socket, with each read or write blocking for at most `slice` and never past
/// `deadline`, so a host that paces its bytes cannot keep a call going.
struct Timed<'a> {
    stream: &'a UnixStream,
    deadline: Instant,
    slice: Option<Duration>,
}

impl Timed<'_> {
    fn wait(&self) -> io::Result<Duration> {
        let left = self.deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(timed_out());
        }
        Ok(self.slice.map_or(left, |s| s.min(left)))
    }
}

impl Read for Timed<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.stream.set_read_timeout(Some(self.wait()?))?;
        (&mut &*self.stream).read(buf)
    }
}

impl Write for Timed<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.stream.set_write_timeout(Some(self.wait()?))?;
        (&mut &*self.stream).write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Watches a host's header for a main thread busy in the plugin's editor.
struct Watch<'a> {
    header: Option<&'a Header>,
    /// The pump the host was in when last looked at, and since when.
    seen: Option<(u32, Instant)>,
}

impl Watch<'_> {
    /// Whether the host has been in the same pump, with the editor open, for
    /// [`EDITOR_BUSY_AFTER`].
    fn busy(&mut self) -> bool {
        let Some(pump) = self.header.and_then(Header::pumping_editor) else {
            self.seen = None;
            return false;
        };
        match self.seen {
            Some((seen, since)) if seen == pump => since.elapsed() >= EDITOR_BUSY_AFTER,
            _ => {
                self.seen = Some((pump, Instant::now()));
                false
            }
        }
    }
}

fn is_timeout(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}

fn timed_out() -> io::Error {
    io::Error::new(
        io::ErrorKind::TimedOut,
        "the plugin host did not answer in time",
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::Ordering;
    use std::thread::JoinHandle;

    use super::*;
    use crate::protocol::Reply;
    use crate::shm::Shm;

    /// A host that answers requests as the real one does, claiming each in `shm` first. It
    /// reads nothing while `shm` says its main thread is in a pump. Returns the requests it
    /// ran; each is answered with its number.
    fn host(stream: UnixStream, shm: Arc<Shm>) -> JoinHandle<Vec<String>> {
        std::thread::spawn(move || {
            let h = shm.header();
            let mut reader = crate::protocol::FrameReader::default();
            let mut ran = Vec::new();
            let mut n = 0;
            stream
                .set_read_timeout(Some(Duration::from_millis(5)))
                .unwrap();
            loop {
                if h.pumps.load(Ordering::SeqCst) % 2 == 1 {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                }
                let (request, _): (Request, _) = match reader.poll(&stream) {
                    Ok(Some(frame)) => frame,
                    Ok(None) => continue,
                    Err(_) => return ran,
                };
                n += 1;
                let reply = if h.start_request(n) {
                    ran.push(format!("{n} {request:?}"));
                    Reply::Value(n as f64)
                } else {
                    Reply::Withdrawn
                };
                if write_frame(&stream, &Response { latency: 0, reply }, &[]).is_err() {
                    return ran;
                }
            }
        })
    }

    /// A control channel to a [`host`] whose main thread is stuck in a pump with the editor
    /// open, as while the plugin shows a modal dialog.
    fn busy_host() -> (Control, Arc<Shm>, JoinHandle<Vec<String>>) {
        let shm = Shm::scratch();
        shm.header().editor_open.store(1, Ordering::Release);
        shm.header().pumps.store(1, Ordering::SeqCst);
        let (app, peer) = UnixStream::pair().unwrap();
        let host = host(peer, shm.clone());
        (Control::new(app), shm, host)
    }

    fn value(result: Result<(Response, Vec<u8>), CallError>) -> f64 {
        match result.unwrap().0.reply {
            Reply::Value(v) => v,
            other => panic!("{other:?}"),
        }
    }

    const LONG: Duration = Duration::from_secs(30);

    #[test]
    fn a_call_withdraws_from_a_host_busy_in_the_editor_and_leaves_it_running() {
        let (control, shm, host) = busy_host();
        let h = Some(shm.header());

        let start = Instant::now();
        let result = control.call(&Request::ParamValue(7), &[], LONG, h);
        assert!(matches!(result, Err(CallError::Busy)), "{result:?}");
        let waited = start.elapsed();
        assert!(waited >= EDITOR_BUSY_AFTER, "{waited:?}");
        assert!(waited < Duration::from_secs(5), "{waited:?}");

        // While the same pump lasts, calls fail at once and send nothing.
        let start = Instant::now();
        let result = control.call(&Request::SaveState, &[], LONG, h);
        assert!(matches!(result, Err(CallError::Busy)), "{result:?}");
        assert!(start.elapsed() < EDITOR_BUSY_AFTER, "{:?}", start.elapsed());

        // The dialog closes. The withdrawn request does not run, and its answer does not
        // pass for the next one's.
        shm.header().pumps.store(2, Ordering::SeqCst);
        assert_eq!(value(control.call(&Request::Params, &[], LONG, h)), 2.0);
        drop(control);
        assert_eq!(host.join().unwrap(), ["2 Params"]);
    }

    #[test]
    fn a_request_that_can_wait_runs_once_the_host_is_free() {
        let (control, shm, host) = busy_host();
        let h = Some(shm.header());
        let result = control.call(&Request::CloseEditor, &[], LONG, h);
        assert!(matches!(result, Err(CallError::Busy)), "{result:?}");

        shm.header().pumps.store(2, Ordering::SeqCst);
        assert_eq!(value(control.call(&Request::Params, &[], LONG, h)), 2.0);
        drop(control);
        assert_eq!(host.join().unwrap(), ["1 CloseEditor", "2 Params"]);
    }

    #[test]
    fn a_large_request_cut_off_by_a_busy_host_is_finished_later() {
        let (control, shm, host) = busy_host();
        let h = Some(shm.header());
        // Far more than the socket buffers hold.
        let state = vec![7u8; 8 << 20];
        let result = control.call(&Request::LoadState, &state, LONG, h);
        assert!(matches!(result, Err(CallError::Busy)), "{result:?}");
        assert!(
            !control.unsent.borrow().is_empty(),
            "the state fit in the socket"
        );

        shm.header().pumps.store(2, Ordering::SeqCst);
        assert_eq!(value(control.call(&Request::Params, &[], LONG, h)), 2.0);
        drop(control);
        assert_eq!(host.join().unwrap(), ["2 Params"]);
    }

    /// With the editor closed, a plugin has no business keeping its main thread: that is a
    /// hang, and the call times out as before.
    #[test]
    fn a_host_stuck_in_a_pump_without_its_editor_times_out() {
        let (control, shm, host) = busy_host();
        shm.header().editor_open.store(0, Ordering::Release);
        let timeout = EDITOR_BUSY_AFTER * 2;
        let result = control.call(&Request::Params, &[], timeout, Some(shm.header()));
        match result {
            Err(CallError::Io(e)) => assert_eq!(e.kind(), io::ErrorKind::TimedOut),
            other => panic!("{other:?}"),
        }
        drop(control);
        shm.header().pumps.store(2, Ordering::SeqCst);
        host.join().unwrap();
    }

    #[test]
    fn a_host_that_trickles_its_reply_cannot_outlast_the_call_timeout() {
        let (app, mut host) = UnixStream::pair().unwrap();
        let trickle = std::thread::spawn(move || {
            let mut request = [0u8; 256];
            let _ = host.read(&mut request);
            // Announce a large reply, then send it a byte at a time, each well inside the
            // timeout, for far longer than the timeout.
            let mut head = Vec::new();
            head.extend_from_slice(&4096u32.to_le_bytes());
            head.extend_from_slice(&(64u32 << 20).to_le_bytes());
            if host.write_all(&head).is_err() {
                return;
            }
            for _ in 0..400 {
                std::thread::sleep(Duration::from_millis(50));
                if host.write_all(b" ").is_err() {
                    return;
                }
            }
        });
        let app = Control::new(app);
        let start = Instant::now();
        let result = app.call(&Request::Params, &[], Duration::from_millis(200), None);
        let elapsed = start.elapsed();
        drop(app);
        trickle.join().unwrap();
        assert!(result.is_err());
        // The trickle lasts 20 s; the call must give up long before.
        assert!(elapsed < Duration::from_secs(10), "{elapsed:?}");
    }
}
