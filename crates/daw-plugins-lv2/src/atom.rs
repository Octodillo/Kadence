//! Small RT-safe atom writers and the URIDs the host needs.

use std::ffi::CStr;

/// URIDs mapped once at instantiation so the audio thread never maps.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Urids {
    pub midi_event: u32,
    pub atom_object: u32,
    pub atom_long: u32,
    pub atom_int: u32,
    pub atom_float: u32,
    pub event_transfer: u32,
    pub time_position: u32,
    pub time_frame: u32,
    pub time_speed: u32,
    pub time_bpm: u32,
    pub time_bar: u32,
    pub time_bar_beat: u32,
    pub time_beats_per_bar: u32,
    pub time_beat_unit: u32,
}

impl Urids {
    pub(crate) fn new(f: &livi::Features) -> Self {
        let m = |uri: &CStr| f.urid(uri);
        Self {
            midi_event: f.midi_urid(),
            atom_object: m(c"http://lv2plug.in/ns/ext/atom#Object"),
            atom_long: m(c"http://lv2plug.in/ns/ext/atom#Long"),
            atom_int: m(c"http://lv2plug.in/ns/ext/atom#Int"),
            atom_float: m(c"http://lv2plug.in/ns/ext/atom#Float"),
            event_transfer: m(c"http://lv2plug.in/ns/ext/atom#eventTransfer"),
            time_position: m(c"http://lv2plug.in/ns/ext/time#Position"),
            time_frame: m(c"http://lv2plug.in/ns/ext/time#frame"),
            time_speed: m(c"http://lv2plug.in/ns/ext/time#speed"),
            time_bpm: m(c"http://lv2plug.in/ns/ext/time#beatsPerMinute"),
            time_bar: m(c"http://lv2plug.in/ns/ext/time#bar"),
            time_bar_beat: m(c"http://lv2plug.in/ns/ext/time#barBeat"),
            time_beats_per_bar: m(c"http://lv2plug.in/ns/ext/time#beatsPerBar"),
            time_beat_unit: m(c"http://lv2plug.in/ns/ext/time#beatUnit"),
        }
    }
}

/// Event header: time (8), size (4), type (4).
const EVENT_HEADER: usize = 16;

/// Bytes an event with a `body_len`-byte body takes in a sequence.
pub(crate) fn event_size(body_len: usize) -> usize {
    (EVENT_HEADER + body_len).next_multiple_of(8)
}

/// Appends an event to an atom sequence without a size limit of its own (the
/// sequence's capacity is the limit). Returns false if it does not fit.
pub(crate) fn push_event(
    seq: &mut livi::event::LV2AtomSequence,
    time: i64,
    ty: u32,
    body: &[u8],
) -> bool {
    let total = seq.capacity() + std::mem::size_of::<lv2_raw::LV2AtomSequence>();
    let used = seq.size();
    let padded = event_size(body.len());
    if used + padded > total {
        return false;
    }
    let base = seq.as_mut_ptr();
    // SAFETY: the write stays inside the sequence's buffer (checked above), and
    // the sequence header's size is updated to cover the new, padded event.
    // livi (and the plugin) already rely on the heap buffer being aligned
    // for the sequence header, as malloc guarantees.
    unsafe {
        let dst = base.cast::<u8>().add(used);
        std::ptr::copy_nonoverlapping(time.to_ne_bytes().as_ptr(), dst, 8);
        std::ptr::copy_nonoverlapping((body.len() as u32).to_ne_bytes().as_ptr(), dst.add(8), 4);
        std::ptr::copy_nonoverlapping(ty.to_ne_bytes().as_ptr(), dst.add(12), 4);
        std::ptr::copy_nonoverlapping(body.as_ptr(), dst.add(EVENT_HEADER), body.len());
        (*base).atom.size += padded as u32;
    }
    true
}

/// Writes the body of an `atom:Object` into a fixed buffer, one property at a
/// time. The atom header (size and type) is not included: the event builder
/// adds it.
pub(crate) struct ObjectWriter<'a> {
    buf: &'a mut [u8],
    len: usize,
}

impl<'a> ObjectWriter<'a> {
    pub(crate) fn new(buf: &'a mut [u8], otype: u32) -> Self {
        let mut w = Self { buf, len: 0 };
        w.put_u32(0); // id: blank node
        w.put_u32(otype);
        w
    }

    fn put_u32(&mut self, v: u32) {
        self.buf[self.len..self.len + 4].copy_from_slice(&v.to_ne_bytes());
        self.len += 4;
    }

    fn property(&mut self, key: u32, ty: u32, value: &[u8]) {
        self.put_u32(key);
        self.put_u32(0); // context
        self.put_u32(value.len() as u32);
        self.put_u32(ty);
        self.buf[self.len..self.len + value.len()].copy_from_slice(value);
        self.len += value.len();
        // Every property is padded to 64 bits.
        let padded = self.len.next_multiple_of(8);
        self.buf[self.len..padded].fill(0);
        self.len = padded;
    }

    pub(crate) fn long(&mut self, key: u32, ty: u32, v: i64) {
        self.property(key, ty, &v.to_ne_bytes());
    }

    pub(crate) fn int(&mut self, key: u32, ty: u32, v: i32) {
        self.property(key, ty, &v.to_ne_bytes());
    }

    pub(crate) fn float(&mut self, key: u32, ty: u32, v: f32) {
        self.property(key, ty, &v.to_ne_bytes());
    }

    pub(crate) fn finish(self) -> usize {
        self.len
    }
}

/// The host's transport as a `time:Position` object body, in the shape jalv
/// and Ardour send. The contract's transport has no musical position, so bars
/// and beats assume a constant tempo in 4/4 from the project start.
pub(crate) fn time_position(
    u: &Urids,
    buf: &mut [u8; 192],
    position: i64,
    playing: bool,
    bpm: f64,
    sample_rate: f64,
) -> usize {
    let beats = if sample_rate > 0.0 {
        position as f64 / sample_rate * bpm / 60.0
    } else {
        0.0
    };
    let bar = (beats / 4.0).floor();
    let mut w = ObjectWriter::new(buf, u.time_position);
    w.long(u.time_frame, u.atom_long, position);
    w.float(u.time_speed, u.atom_float, if playing { 1.0 } else { 0.0 });
    w.float(u.time_bpm, u.atom_float, bpm as f32);
    w.long(u.time_bar, u.atom_long, bar as i64);
    w.float(u.time_bar_beat, u.atom_float, (beats - bar * 4.0) as f32);
    w.float(u.time_beats_per_bar, u.atom_float, 4.0);
    w.int(u.time_beat_unit, u.atom_int, 4);
    w.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u32_at(b: &[u8], at: usize) -> u32 {
        u32::from_ne_bytes(b[at..at + 4].try_into().unwrap())
    }

    #[test]
    fn object_properties_are_padded_and_typed() {
        let mut buf = [0xaa_u8; 64];
        let mut w = ObjectWriter::new(&mut buf, 7);
        w.float(1, 2, 0.5);
        w.long(3, 4, -9);
        let len = w.finish();
        // header 8, float prop 16 + 4 padded to 24, long prop 16 + 8.
        assert_eq!(len, 8 + 24 + 24);
        assert_eq!(u32_at(&buf, 4), 7);
        assert_eq!(u32_at(&buf, 8), 1);
        assert_eq!(u32_at(&buf, 16), 4); // value size
        assert_eq!(u32_at(&buf, 20), 2); // value type
        assert_eq!(f32::from_ne_bytes(buf[24..28].try_into().unwrap()), 0.5);
        assert_eq!(&buf[28..32], &[0, 0, 0, 0]);
        assert_eq!(u32_at(&buf, 32), 3);
        assert_eq!(i64::from_ne_bytes(buf[48..56].try_into().unwrap()), -9);
    }

    #[test]
    fn a_full_position_fits_its_buffer() {
        let u = Urids {
            midi_event: 1,
            atom_object: 2,
            atom_long: 4,
            atom_int: 5,
            atom_float: 6,
            event_transfer: 7,
            time_position: 8,
            time_frame: 9,
            time_speed: 10,
            time_bpm: 11,
            time_bar: 12,
            time_bar_beat: 13,
            time_beats_per_bar: 14,
            time_beat_unit: 15,
        };
        let mut buf = [0; 192];
        // 2 bars and 1 beat at 120 bpm, 48 kHz: 9 beats = 4.5 s.
        let len = time_position(&u, &mut buf, 216_000, true, 120.0, 48_000.0);
        assert!(len <= buf.len());
        assert_eq!(len % 8, 0);
    }
}
