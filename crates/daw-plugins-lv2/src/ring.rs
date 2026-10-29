//! Framed messages over a lock-free byte ring between the UI (main thread) and
//! the audio thread. Writing and reading never allocate.

use rtrb::{Consumer, Producer, RingBuffer};

/// A float written to a control port (protocol 0).
pub(crate) const CONTROL: u8 = 0;
/// A whole atom (header and body) for or from an atom port (`atom:eventTransfer`).
pub(crate) const ATOM: u8 = 1;

const HEADER: usize = 9;
pub(crate) const CAPACITY: usize = 64 * 1024;

pub(crate) fn channel() -> (Producer<u8>, Consumer<u8>) {
    RingBuffer::new(CAPACITY)
}

/// Queues one message. Returns false, dropping it, when the ring is full.
pub(crate) fn write(p: &mut Producer<u8>, port: u32, kind: u8, body: &[u8]) -> bool {
    write_parts(p, port, kind, body, &[])
}

/// Like [`write`], with the body given in two pieces.
pub(crate) fn write_parts(
    p: &mut Producer<u8>,
    port: u32,
    kind: u8,
    first: &[u8],
    second: &[u8],
) -> bool {
    let len = first.len() + second.len();
    let total = HEADER + len;
    let Ok(mut chunk) = p.write_chunk(total) else {
        return false;
    };
    let mut header = [0u8; HEADER];
    header[..4].copy_from_slice(&port.to_le_bytes());
    header[4] = kind;
    header[5..].copy_from_slice(&(len as u32).to_le_bytes());
    let (a, b) = chunk.as_mut_slices();
    let mut src = header.iter().chain(first).chain(second);
    for (dst, s) in a.iter_mut().chain(b.iter_mut()).zip(&mut src) {
        *dst = *s;
    }
    chunk.commit_all();
    true
}

pub(crate) struct Message {
    pub port: u32,
    pub kind: u8,
    /// Bytes copied into the caller's buffer. A message bigger than the buffer
    /// is consumed and reported with `len == None`.
    pub len: Option<usize>,
}

/// The next message's port, kind and body size, without consuming it.
pub(crate) fn peek(c: &mut Consumer<u8>) -> Option<(u32, u8, usize)> {
    let chunk = c.read_chunk(HEADER).ok()?;
    let mut header = [0u8; HEADER];
    let (a, b) = chunk.as_slices();
    for (dst, s) in header.iter_mut().zip(a.iter().chain(b.iter())) {
        *dst = *s;
    }
    // Dropped without committing: nothing is consumed.
    Some((
        u32::from_le_bytes(header[..4].try_into().unwrap()),
        header[4],
        u32::from_le_bytes(header[5..].try_into().unwrap()) as usize,
    ))
}

/// Pops the next message into `buf`.
pub(crate) fn read(c: &mut Consumer<u8>, buf: &mut [u8]) -> Option<Message> {
    let mut header = [0u8; HEADER];
    {
        let chunk = c.read_chunk(HEADER).ok()?;
        let (a, b) = chunk.as_slices();
        for (dst, s) in header.iter_mut().zip(a.iter().chain(b.iter())) {
            *dst = *s;
        }
        // Not committed: the header is consumed together with the body below.
    }
    let port = u32::from_le_bytes(header[..4].try_into().unwrap());
    let kind = header[4];
    let size = u32::from_le_bytes(header[5..].try_into().unwrap()) as usize;
    // The writer commits header and body at once, so the body is there.
    let chunk = c.read_chunk(HEADER + size).ok()?;
    let len = if size <= buf.len() {
        let (a, b) = chunk.as_slices();
        for (dst, s) in buf.iter_mut().zip(a.iter().chain(b.iter()).skip(HEADER)) {
            *dst = *s;
        }
        Some(size)
    } else {
        None
    };
    chunk.commit_all();
    Some(Message { port, kind, len })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_survive_wraparound_and_oversize_is_skipped() {
        let (mut p, mut c) = channel();
        let mut buf = [0u8; 16];
        for round in 0..20_000u32 {
            let body = round.to_le_bytes();
            assert!(write(&mut p, round, CONTROL, &body));
            let m = read(&mut c, &mut buf).unwrap();
            assert_eq!((m.port, m.kind, m.len), (round, CONTROL, Some(4)));
            assert_eq!(&buf[..4], &body);
        }
        assert!(write(&mut p, 1, ATOM, &[7; 100]));
        assert!(write(&mut p, 2, ATOM, &[8; 3]));
        assert_eq!(read(&mut c, &mut buf).unwrap().len, None);
        let m = read(&mut c, &mut buf).unwrap();
        assert_eq!((m.port, m.len), (2, Some(3)));
        assert!(read(&mut c, &mut buf).is_none());
    }

    #[test]
    fn a_full_ring_drops_instead_of_blocking() {
        let (mut p, _c) = channel();
        let body = [0u8; 1000];
        let mut sent = 0;
        while write(&mut p, 0, ATOM, &body) {
            sent += 1;
        }
        assert_eq!(sent, CAPACITY / (HEADER + body.len()));
    }
}
