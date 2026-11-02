//! The application's end of the control socket to one host process.
//!
//! Requests and replies alternate, and both sides number the requests in the order they are
//! sent. The host's main thread reads them between its pumps of the plugin's main-thread
//! callbacks, so a plugin that stays in one (its editor showing a modal file dialog, say)
//! cannot answer until the user is done. Such a host is busy, not hung: when a call finds it
//! [waiting on the user](Header::waiting_on_user), the call gives up with [`CallError::Busy`]
//! and leaves the host running. It withdraws its request through the audio block's header, so
//! the host answers it without running it, unless the request [can wait](Request::can_wait),
//! which the host then runs once it is free. Either way the reply still comes. The next call
//! first finishes writing the request, if the host stopped reading it halfway, and reads that
//! reply: only then does it send its own, so at most one request is ever outstanding.

use std::cell::{Cell, RefCell};
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use crate::protocol::{FrameReader, Request, Response, write_frame};
use crate::shm::Header;

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
    /// What is left to write of the last request: a call that gave up on a busy host can
    /// leave part of it behind.
    unsent: RefCell<Vec<u8>>,
    /// Requests sent so far, the last one's number.
    sent: Cell<u64>,
    /// The last request was given up on, and its reply is still to come.
    owed: Cell<bool>,
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
            owed: Cell::new(false),
            busy_in: Cell::new(None),
        }
    }

    /// Sends `request` and returns its reply, all within `timeout`, however the host paces its
    /// bytes. With `header`, the block the host uses now, a host found waiting on the user in
    /// the plugin's editor makes the call return [`CallError::Busy`] instead of waiting it out.
    pub(crate) fn call(
        &self,
        request: &Request,
        blob: &[u8],
        timeout: Duration,
        header: Option<&Header>,
    ) -> Result<(Response, Vec<u8>), CallError> {
        let watch = Watch { header };
        if let Some(busy) = self.busy_in.get() {
            if header.and_then(Header::pumping_editor).map(|p| p.id) == Some(busy) {
                return Err(CallError::Busy);
            }
            self.busy_in.set(None);
        }
        let deadline = Instant::now() + timeout.max(Duration::from_millis(1));

        // The last request, given up on, is finished first: until the host has answered it,
        // this one is not sent at all.
        if let Err(e) = self.flush(&watch, deadline) {
            return Err(self.note_busy(e, &watch));
        }
        while self.owed.get() {
            match self.read_frame(&watch, deadline)? {
                Some(_) => self.owed.set(false),
                None => self
                    .check(&watch, deadline)
                    .map_err(|e| self.note_busy(e, &watch))?,
            }
        }

        let n = self.sent.get() + 1;
        self.sent.set(n);
        write_frame(&mut *self.unsent.borrow_mut(), request, blob)?;
        if let Err(e) = self.flush(&watch, deadline) {
            if matches!(e, CallError::Busy) {
                // The host has not read all of it, so cannot have started it.
                let given_up = self.give_up(n, request, &watch);
                debug_assert!(given_up, "a request the host has not read was started");
            }
            return Err(self.note_busy(e, &watch));
        }
        loop {
            if let Some(frame) = self.read_frame(&watch, deadline)? {
                return Ok(frame);
            }
            match self.check(&watch, deadline) {
                Ok(()) => {}
                Err(CallError::Busy) if self.give_up(n, request, &watch) => {
                    return Err(self.note_busy(CallError::Busy, &watch));
                }
                // A host that started the request answers it before it pumps again, so the
                // reply is on its way: wait for it, until the deadline.
                Err(CallError::Busy) if Instant::now() < deadline => {}
                Err(CallError::Busy) => return Err(CallError::Io(timed_out())),
                Err(e) => return Err(e),
            }
        }
    }

    /// Between reads: fails with [`CallError::Busy`] if the host waits on the user, or with a
    /// timeout once `deadline` has passed.
    fn check(&self, watch: &Watch<'_>, deadline: Instant) -> Result<(), CallError> {
        if watch.busy() {
            return Err(CallError::Busy);
        }
        if Instant::now() >= deadline {
            return Err(CallError::Io(timed_out()));
        }
        Ok(())
    }

    /// Remembers the pump a call gives up in, so later calls in it fail at once.
    fn note_busy(&self, e: CallError, watch: &Watch<'_>) -> CallError {
        if matches!(e, CallError::Busy) {
            let pump = watch.header.and_then(Header::pumping_editor);
            self.busy_in.set(pump.map(|p| p.id));
        }
        e
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
        self.owed.set(given_up);
        given_up
    }

    /// Writes what is unsent. Fails with [`CallError::Busy`] if the host stops reading because
    /// it is busy, leaving the rest unsent.
    fn flush(&self, watch: &Watch<'_>, deadline: Instant) -> Result<(), CallError> {
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

/// Watches a host's header for a main thread waiting on the user in the plugin's editor.
struct Watch<'a> {
    header: Option<&'a Header>,
}

impl Watch<'_> {
    fn busy(&self) -> bool {
        self.header.and_then(Header::waiting_on_user).is_some()
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
    use crate::shm::{EDITOR_BUSY_AFTER, Shm};

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
                if h.in_pump().is_some() {
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
        shm.header().enter_pump();
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
        shm.header().leave_pump();
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

        shm.header().leave_pump();
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

        shm.header().leave_pump();
        assert_eq!(value(control.call(&Request::Params, &[], LONG, h)), 2.0);
        drop(control);
        assert_eq!(host.join().unwrap(), ["2 Params"]);
    }

    /// The host stops reading a large request it is not going to run, and the user opens
    /// another dialog before it has read the rest. The next request waits for the first to be
    /// answered, so the host never runs the first one in its place.
    #[test]
    fn a_request_given_up_on_is_answered_before_the_next_goes_out() {
        let (control, shm, host) = busy_host();
        let h = Some(shm.header());
        // More than the socket holds, so part is left over; little enough that the rest fits once
        // the host has read the first part.
        let state = vec![7u8; 300 << 10];
        let result = control.call(&Request::LoadState, &state, LONG, h);
        assert!(matches!(result, Err(CallError::Busy)), "{result:?}");
        assert!(
            !control.unsent.borrow().is_empty(),
            "the state fit in the socket"
        );

        // The host reads what has arrived, then the user is back in a dialog.
        shm.header().leave_pump();
        std::thread::sleep(Duration::from_millis(50));
        shm.header().enter_pump_long_ago();
        // Until the host has given up reading too: it pumps only between reads.
        std::thread::sleep(Duration::from_millis(30));
        let result = control.call(&Request::Params, &[], LONG, h);
        assert!(matches!(result, Err(CallError::Busy)), "{result:?}");

        shm.header().leave_pump();
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
        shm.header().leave_pump();
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
