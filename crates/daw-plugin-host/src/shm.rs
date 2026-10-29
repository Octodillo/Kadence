//! The shared-memory block one sandboxed plugin's audio and events go through, and the futex
//! that hands each audio cycle between the application and the host process.
//!
//! One cycle: the application writes inputs, events and transport while the state word is not
//! [`REQUEST`], stores [`REQUEST`] and wakes the host. The host processes, writes outputs and
//! events, stores [`DONE`] and wakes the application. Each side touches the buffers only while
//! the state word says it is that side's turn, and the state word is stored with `Release` and
//! loaded with `Acquire`, so the buffers need no other synchronisation.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

use daw_core::event::{Event, EventKind};
use memmap2::MmapMut;

const MAGIC: u32 = u32::from_le_bytes(*b"KDSB");
const VERSION: u32 = 1;
/// Bytes reserved for [`Header`] at the start of the block.
const HEADER_BYTES: usize = 128;
/// `u64` words per event record.
const EVENT_WORDS: usize = 4;

/// Events each direction can carry per cycle; more are dropped.
pub const EVENT_CAPACITY: u32 = 1024;
/// The most audio channels a sandboxed plugin can have in each direction.
pub const MAX_CHANNELS: usize = 64;

/// State word values.
pub const IDLE: u32 = 0;
pub const REQUEST: u32 = 1;
pub const DONE: u32 = 2;

/// Header flag: call `Node::reset` before processing this cycle.
pub const FLAG_RESET: u32 = 1;

/// The start of the block. Every field is atomic because two processes share it; only `state`
/// orders the rest.
#[repr(C)]
pub struct Header {
    magic: AtomicU32,
    version: AtomicU32,
    audio_in: AtomicU32,
    audio_out: AtomicU32,
    max_frames: AtomicU32,
    event_capacity: AtomicU32,
    /// The futex word: [`IDLE`], [`REQUEST`] or [`DONE`].
    pub state: AtomicU32,
    pub frames: AtomicU32,
    pub flags: AtomicU32,
    pub playing: AtomicU32,
    pub events_in: AtomicU32,
    pub events_out: AtomicU32,
    pub position: AtomicU64,
    /// `f64` bits.
    pub bpm: AtomicU64,
    /// `f64` bits.
    pub sample_rate: AtomicU64,
    /// Cycles the host has finished, for the hang watchdog.
    pub completed: AtomicU64,
}

const _: () = assert!(size_of::<Header>() <= HEADER_BYTES);

/// The sizes a block is made for.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    pub audio_in: u32,
    pub audio_out: u32,
    pub max_frames: u32,
    pub event_capacity: u32,
}

impl Layout {
    fn channel_bytes(&self) -> usize {
        self.max_frames as usize * size_of::<f32>()
    }

    fn audio_in_offset(&self) -> usize {
        HEADER_BYTES
    }

    fn audio_out_offset(&self) -> usize {
        self.audio_in_offset() + self.audio_in as usize * self.channel_bytes()
    }

    fn events_in_offset(&self) -> usize {
        (self.audio_out_offset() + self.audio_out as usize * self.channel_bytes())
            .next_multiple_of(8)
    }

    fn events_out_offset(&self) -> usize {
        self.events_in_offset() + self.event_bytes()
    }

    fn event_bytes(&self) -> usize {
        self.event_capacity as usize * EVENT_WORDS * size_of::<u64>()
    }

    pub fn size(&self) -> usize {
        self.events_out_offset() + self.event_bytes()
    }
}

/// A mapped block. Both processes map the same file.
pub struct Shm {
    // Keeps the mapping alive; all access goes through `ptr`.
    _map: MmapMut,
    ptr: *mut u8,
    layout: Layout,
}

// SAFETY: the block is plain shared memory. Header fields are atomics; the buffers are only
// touched by the side whose turn the state word says it is (see the module docs).
unsafe impl Send for Shm {}
// SAFETY: as above.
unsafe impl Sync for Shm {}

impl Shm {
    /// Creates a new block file in `dir` and maps it. The caller removes the file once the host
    /// has mapped it too.
    pub fn create(dir: &Path, layout: Layout) -> io::Result<(Self, PathBuf)> {
        if layout.audio_in as usize > MAX_CHANNELS || layout.audio_out as usize > MAX_CHANNELS {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("plugins with more than {MAX_CHANNELS} channels cannot be sandboxed"),
            ));
        }
        let (file, path) = create_unique(dir)?;
        let result = (|| {
            file.set_len(layout.size() as u64)?;
            let shm = Self::map(&file, layout)?;
            let h = shm.header();
            h.audio_in.store(layout.audio_in, Ordering::Relaxed);
            h.audio_out.store(layout.audio_out, Ordering::Relaxed);
            h.max_frames.store(layout.max_frames, Ordering::Relaxed);
            h.event_capacity
                .store(layout.event_capacity, Ordering::Relaxed);
            h.version.store(VERSION, Ordering::Relaxed);
            h.state.store(IDLE, Ordering::Relaxed);
            h.magic.store(MAGIC, Ordering::Release);
            Ok(shm)
        })();
        match result {
            Ok(shm) => Ok((shm, path)),
            Err(e) => {
                let _ = std::fs::remove_file(&path);
                Err(e)
            }
        }
    }

    /// Maps a block another process created, checking that it is one.
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        let len = file.metadata()?.len() as usize;
        let bad = |what: &str| io::Error::new(io::ErrorKind::InvalidData, what.to_string());
        if len < HEADER_BYTES {
            return Err(bad("shared memory block too small"));
        }
        // Map the header alone first to read the layout.
        let probe = Self::map(
            &file,
            Layout {
                audio_in: 0,
                audio_out: 0,
                max_frames: 0,
                event_capacity: 0,
            },
        )?;
        let h = probe.header();
        if h.magic.load(Ordering::Acquire) != MAGIC || h.version.load(Ordering::Relaxed) != VERSION
        {
            return Err(bad("not a plugin sandbox block of this version"));
        }
        let layout = Layout {
            audio_in: h.audio_in.load(Ordering::Relaxed),
            audio_out: h.audio_out.load(Ordering::Relaxed),
            max_frames: h.max_frames.load(Ordering::Relaxed),
            event_capacity: h.event_capacity.load(Ordering::Relaxed),
        };
        if layout.audio_in as usize > MAX_CHANNELS
            || layout.audio_out as usize > MAX_CHANNELS
            || layout.size() > len
        {
            return Err(bad("shared memory block layout does not fit"));
        }
        drop(probe);
        Self::map(&file, layout)
    }

    fn map(file: &File, layout: Layout) -> io::Result<Self> {
        let size = layout.size();
        // SAFETY: the file is ours and only ever mapped by the two processes of one sandbox,
        // which access it as the module docs describe.
        let mut map = unsafe { memmap2::MmapOptions::new().len(size).map_mut(file)? };
        let ptr = map.as_mut_ptr();
        Ok(Self {
            _map: map,
            ptr,
            layout,
        })
    }

    pub fn layout(&self) -> Layout {
        self.layout
    }

    pub fn header(&self) -> &Header {
        // SAFETY: the mapping is page aligned and at least HEADER_BYTES long, and Header is all
        // atomics, valid for any bit pattern.
        unsafe { &*self.ptr.cast::<Header>() }
    }

    /// Input channel `ch`, `frames` long.
    ///
    /// # Safety
    /// The caller must own the buffers (see the module docs), must not hold another reference
    /// to the same channel, and `ch < audio_in`, `frames <= max_frames`.
    #[allow(clippy::mut_from_ref)]
    pub unsafe fn audio_in(&self, ch: usize, frames: usize) -> &mut [f32] {
        debug_assert!(ch < self.layout.audio_in as usize);
        debug_assert!(frames <= self.layout.max_frames as usize);
        let offset = self.layout.audio_in_offset() + ch * self.layout.channel_bytes();
        // SAFETY: in bounds by the layout, f32-aligned, and exclusive per the caller's promise.
        unsafe { std::slice::from_raw_parts_mut(self.ptr.add(offset).cast(), frames) }
    }

    /// Output channel `ch`, `frames` long.
    ///
    /// # Safety
    /// As for [`Shm::audio_in`], with `ch < audio_out`.
    #[allow(clippy::mut_from_ref)]
    pub unsafe fn audio_out(&self, ch: usize, frames: usize) -> &mut [f32] {
        debug_assert!(ch < self.layout.audio_out as usize);
        debug_assert!(frames <= self.layout.max_frames as usize);
        let offset = self.layout.audio_out_offset() + ch * self.layout.channel_bytes();
        // SAFETY: as in `audio_in`.
        unsafe { std::slice::from_raw_parts_mut(self.ptr.add(offset).cast(), frames) }
    }

    /// The input event records.
    ///
    /// # Safety
    /// The caller must own the buffers and not hold another reference to them.
    #[allow(clippy::mut_from_ref)]
    pub unsafe fn events_in(&self) -> &mut [u64] {
        // SAFETY: in bounds, 8-aligned, exclusive per the caller's promise.
        unsafe { self.events(self.layout.events_in_offset()) }
    }

    /// The output event records.
    ///
    /// # Safety
    /// As for [`Shm::events_in`].
    #[allow(clippy::mut_from_ref)]
    pub unsafe fn events_out(&self) -> &mut [u64] {
        // SAFETY: as above.
        unsafe { self.events(self.layout.events_out_offset()) }
    }

    #[allow(clippy::mut_from_ref)]
    unsafe fn events(&self, offset: usize) -> &mut [u64] {
        let words = self.layout.event_capacity as usize * EVENT_WORDS;
        // SAFETY: the callers' promises; the offset is 8-aligned by the layout.
        unsafe { std::slice::from_raw_parts_mut(self.ptr.add(offset).cast(), words) }
    }
}

fn create_unique(dir: &Path) -> io::Result<(File, PathBuf)> {
    use std::sync::atomic::AtomicUsize;
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    loop {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        let path = dir.join(format!(
            "kadence-plugin-{}-{n}-{nanos:08x}.shm",
            std::process::id()
        ));
        match OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(file) => return Ok((file, path)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
}

/// Where blocks are created: `/dev/shm` if it exists, so they never touch a disk.
pub fn shm_dir() -> PathBuf {
    let dev_shm = Path::new("/dev/shm");
    if dev_shm.is_dir() {
        dev_shm.to_path_buf()
    } else {
        std::env::temp_dir()
    }
}

/// Sleeps while `word` is `expected`, for at most `timeout`. Returns early on a wake, a
/// signal or a spurious wake-up, so callers loop. Real-time safe.
pub fn futex_wait(word: &AtomicU32, expected: u32, timeout: Duration) {
    let ts = libc::timespec {
        tv_sec: timeout.as_secs().min(i64::MAX as u64) as libc::time_t,
        tv_nsec: libc::c_long::from(timeout.subsec_nanos() as i32),
    };
    // SAFETY: FUTEX_WAIT reads the aligned u32 behind the pointer, which stays valid for the
    // call. Not FUTEX_PRIVATE: the word is shared between processes.
    unsafe {
        libc::syscall(
            libc::SYS_futex,
            word.as_ptr(),
            libc::FUTEX_WAIT,
            expected,
            &raw const ts,
            std::ptr::null::<u32>(),
            0u32,
        );
    }
}

/// Wakes every waiter on `word`. Real-time safe.
pub fn futex_wake(word: &AtomicU32) {
    // SAFETY: FUTEX_WAKE only uses the address as a key.
    unsafe {
        libc::syscall(
            libc::SYS_futex,
            word.as_ptr(),
            libc::FUTEX_WAKE,
            i32::MAX,
            std::ptr::null::<libc::timespec>(),
            std::ptr::null::<u32>(),
            0u32,
        );
    }
}

/// Writes `e` into one record of `EVENT_WORDS` words.
pub fn encode_event(e: &Event, out: &mut [u64]) {
    let (tag, a, b, c) = match e.kind {
        EventKind::NoteOn {
            channel,
            key,
            velocity,
            note_id,
        } => (
            1u64,
            u64::from(channel) | u64::from(key) << 8,
            u64::from(velocity.to_bits()),
            u64::from(note_id as u32),
        ),
        EventKind::NoteOff {
            channel,
            key,
            velocity,
            note_id,
        } => (
            2,
            u64::from(channel) | u64::from(key) << 8,
            u64::from(velocity.to_bits()),
            u64::from(note_id as u32),
        ),
        EventKind::Midi([s, d1, d2]) => (
            3,
            u64::from(s) | u64::from(d1) << 8 | u64::from(d2) << 16,
            0,
            0,
        ),
        EventKind::ParamValue { param, value } => (4, u64::from(param), value.to_bits(), 0),
    };
    out[0] = u64::from(e.frame) | tag << 32;
    out[1] = a;
    out[2] = b;
    out[3] = c;
}

/// Reads one record written by [`encode_event`]; `None` for an unknown tag.
pub fn decode_event(w: &[u64]) -> Option<Event> {
    let frame = w[0] as u32;
    let byte = |v: u64, i: u32| (v >> (8 * i)) as u8;
    let kind = match w[0] >> 32 {
        1 => EventKind::NoteOn {
            channel: byte(w[1], 0),
            key: byte(w[1], 1),
            velocity: f32::from_bits(w[2] as u32),
            note_id: w[3] as u32 as i32,
        },
        2 => EventKind::NoteOff {
            channel: byte(w[1], 0),
            key: byte(w[1], 1),
            velocity: f32::from_bits(w[2] as u32),
            note_id: w[3] as u32 as i32,
        },
        3 => EventKind::Midi([byte(w[1], 0), byte(w[1], 1), byte(w[1], 2)]),
        4 => EventKind::ParamValue {
            param: w[1] as u32,
            value: f64::from_bits(w[2]),
        },
        _ => return None,
    };
    Some(Event { frame, kind })
}

/// Event record `i` of a record slice.
pub fn record(words: &mut [u64], i: usize) -> &mut [u64] {
    &mut words[i * EVENT_WORDS..(i + 1) * EVENT_WORDS]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_round_trip() {
        let events = [
            Event {
                frame: 7,
                kind: EventKind::NoteOn {
                    channel: 15,
                    key: 127,
                    velocity: 0.75,
                    note_id: -1,
                },
            },
            Event {
                frame: u32::MAX,
                kind: EventKind::NoteOff {
                    channel: 0,
                    key: 60,
                    velocity: 0.0,
                    note_id: 123_456,
                },
            },
            Event {
                frame: 0,
                kind: EventKind::Midi([0xb0, 7, 100]),
            },
            Event {
                frame: 3,
                kind: EventKind::ParamValue {
                    param: u32::MAX,
                    value: -1234.5e-7,
                },
            },
        ];
        let mut words = [0u64; EVENT_WORDS];
        for e in events {
            encode_event(&e, &mut words);
            assert_eq!(decode_event(&words), Some(e));
        }
        words[0] = 9 << 32;
        assert_eq!(decode_event(&words), None);
    }

    #[test]
    fn a_block_opens_with_its_layout_and_shares_memory() {
        let layout = Layout {
            audio_in: 2,
            audio_out: 3,
            max_frames: 100,
            event_capacity: 16,
        };
        let (a, path) = Shm::create(&std::env::temp_dir(), layout).unwrap();
        let b = Shm::open(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(b.layout(), layout);

        // SAFETY: single-threaded test; no other references to these buffers.
        unsafe {
            a.audio_in(1, 100)[99] = 0.5;
            a.audio_out(2, 100)[0] = -0.25;
            a.events_out()[16 * EVENT_WORDS - 1] = 42;
            assert_eq!(b.audio_in(1, 100)[99], 0.5);
            assert_eq!(b.audio_out(2, 100)[0], -0.25);
            assert_eq!(b.events_out()[16 * EVENT_WORDS - 1], 42);
        }
        a.header().state.store(DONE, Ordering::Release);
        assert_eq!(b.header().state.load(Ordering::Acquire), DONE);
    }

    #[test]
    fn opening_something_else_fails() {
        let path = std::env::temp_dir().join(format!("not-shm-{}", std::process::id()));
        std::fs::write(&path, [0u8; 4096]).unwrap();
        assert!(Shm::open(&path).is_err());
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn too_many_channels_are_refused() {
        let layout = Layout {
            audio_in: MAX_CHANNELS as u32 + 1,
            audio_out: 2,
            max_frames: 64,
            event_capacity: 16,
        };
        assert!(Shm::create(&std::env::temp_dir(), layout).is_err());
    }
}
