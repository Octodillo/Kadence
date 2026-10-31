//! The control channel between the application and a host process: main-thread calls, one
//! request and one response at a time, over a Unix socket.
//!
//! A frame is `[json length: u32 LE][blob length: u32 LE][json][blob]`. The JSON is a
//! [`Request`] or a [`Response`]; the blob carries plugin state, which can be large.

use std::io::{self, Read, Write};
use std::path::PathBuf;

use daw_core::event::ParamId;
use daw_core::node::PortLayout;
use daw_core::plugin::{ParamInfo, PluginDescriptor, PluginError};
use serde::{Deserialize, Serialize};

/// Frames larger than this are refused, so a broken peer cannot make us allocate without bound.
const MAX_FRAME: usize = 512 << 20;

#[derive(Debug, Serialize, Deserialize)]
pub enum Request {
    Instantiate {
        descriptor: PluginDescriptor,
        sample_rate: f64,
        max_frames: u32,
        rt_priority: Option<i32>,
    },
    /// With `shm`, the host maps that block and processes through it from now on. With
    /// `prepare`, it calls `Node::prepare` with the settings; without, it only swaps the block,
    /// for a plugin already prepared with them. Answered with [`Reply::Prepared`].
    Prepare {
        sample_rate: f64,
        max_frames: u32,
        shm: Option<PathBuf>,
        prepare: bool,
    },
    Params,
    ParamValue(ParamId),
    SaveState,
    /// The state is the frame's blob.
    LoadState,
    OpenEditor,
    CloseEditor,
    /// Answered with [`Reply::Ok`].
    SetEditorTitle(String),
    Shutdown,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Response {
    /// The plugin's latency after the call, so changes reach the application.
    pub latency: u32,
    pub reply: Reply,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum Reply {
    Instantiated(InstanceInfo),
    /// The plugin's ports after a `Prepare`, which can change them.
    Prepared(WirePorts),
    Ok,
    Params(Vec<WireParam>),
    Value(f64),
    /// The state is the frame's blob.
    State,
    Err(WireError),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InstanceInfo {
    pub ports: WirePorts,
    pub params: Vec<WireParam>,
    pub has_editor: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WirePorts {
    pub audio_in: u16,
    pub audio_out: u16,
    pub event_in: bool,
    pub event_out: bool,
}

impl From<&PortLayout> for WirePorts {
    fn from(p: &PortLayout) -> Self {
        Self {
            audio_in: p.audio_in,
            audio_out: p.audio_out,
            event_in: p.event_in,
            event_out: p.event_out,
        }
    }
}

impl From<&WirePorts> for PortLayout {
    fn from(p: &WirePorts) -> Self {
        Self {
            audio_in: p.audio_in,
            audio_out: p.audio_out,
            event_in: p.event_in,
            event_out: p.event_out,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WireParam {
    pub id: ParamId,
    pub name: String,
    pub min: f64,
    pub max: f64,
    pub default: f64,
    pub automatable: bool,
}

/// `value` made finite, since JSON has no NaN or infinity: infinities are clamped and NaN
/// becomes `nan`.
fn finite(value: f64, nan: f64) -> f64 {
    if value.is_nan() {
        nan
    } else {
        value.clamp(f64::MIN, f64::MAX)
    }
}

impl From<&ParamInfo> for WireParam {
    fn from(p: &ParamInfo) -> Self {
        let default = finite(p.default, 0.0);
        Self {
            id: p.id,
            name: p.name.clone(),
            min: finite(p.min, default),
            max: finite(p.max, default),
            default,
            automatable: p.automatable,
        }
    }
}

impl From<&WireParam> for ParamInfo {
    fn from(p: &WireParam) -> Self {
        Self {
            id: p.id,
            name: p.name.clone(),
            min: p.min,
            max: p.max,
            default: p.default,
            automatable: p.automatable,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub enum WireError {
    Load(String),
    State(String),
    Editor(String),
}

impl From<&PluginError> for WireError {
    fn from(e: &PluginError) -> Self {
        match e {
            PluginError::Load(m) => Self::Load(m.clone()),
            PluginError::State(m) => Self::State(m.clone()),
            PluginError::Editor(m) => Self::Editor(m.clone()),
        }
    }
}

impl From<WireError> for PluginError {
    fn from(e: WireError) -> Self {
        match e {
            WireError::Load(m) => Self::Load(m),
            WireError::State(m) => Self::State(m),
            WireError::Editor(m) => Self::Editor(m),
        }
    }
}

fn invalid(e: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e.to_string())
}

/// Whether a blob of `len` bytes can be sent.
pub fn blob_fits(len: usize) -> bool {
    len <= MAX_FRAME
}

/// Writes one frame.
pub fn write_frame(mut w: impl Write, msg: &impl Serialize, blob: &[u8]) -> io::Result<()> {
    let json = serde_json::to_vec(msg).map_err(invalid)?;
    if json.len() > MAX_FRAME || blob.len() > MAX_FRAME {
        return Err(invalid("message too large"));
    }
    let mut head = Vec::with_capacity(8 + json.len());
    head.extend_from_slice(&(json.len() as u32).to_le_bytes());
    head.extend_from_slice(&(blob.len() as u32).to_le_bytes());
    head.extend_from_slice(&json);
    w.write_all(&head)?;
    w.write_all(blob)?;
    w.flush()
}

/// Reads one frame, blocking (or until the stream's read timeout).
#[cfg(test)]
pub fn read_frame<T: for<'de> Deserialize<'de>>(mut r: impl Read) -> io::Result<(T, Vec<u8>)> {
    let mut lens = [0u8; 8];
    r.read_exact(&mut lens)?;
    let (json_len, blob_len) = split_lens(&lens)?;
    let mut json = vec![0; json_len];
    r.read_exact(&mut json)?;
    let mut blob = vec![0; blob_len];
    r.read_exact(&mut blob)?;
    Ok((serde_json::from_slice(&json).map_err(invalid)?, blob))
}

fn split_lens(lens: &[u8]) -> io::Result<(usize, usize)> {
    let json = u32::from_le_bytes(lens[..4].try_into().expect("4 bytes")) as usize;
    let blob = u32::from_le_bytes(lens[4..8].try_into().expect("4 bytes")) as usize;
    if json > MAX_FRAME || blob > MAX_FRAME {
        return Err(invalid("message too large"));
    }
    Ok((json, blob))
}

/// Collects frames from a stream with a read timeout, so the reader can do other work (the
/// host pumps the plugin's main thread) between messages without losing a partial frame.
#[derive(Default)]
pub struct FrameReader {
    buf: Vec<u8>,
}

impl FrameReader {
    /// Returns a complete frame if one is buffered or arrives before the read times out,
    /// `Ok(None)` on a timeout, and an error when the peer has gone.
    pub fn poll<T: for<'de> Deserialize<'de>>(
        &mut self,
        mut r: impl Read,
    ) -> io::Result<Option<(T, Vec<u8>)>> {
        loop {
            if let Some(frame) = self.take()? {
                return Ok(Some(frame));
            }
            let mut chunk = [0u8; 64 * 1024];
            match r.read(&mut chunk) {
                Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
                Ok(n) => self.buf.extend_from_slice(&chunk[..n]),
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    return Ok(None);
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
    }

    fn take<T: for<'de> Deserialize<'de>>(&mut self) -> io::Result<Option<(T, Vec<u8>)>> {
        if self.buf.len() < 8 {
            return Ok(None);
        }
        let (json_len, blob_len) = split_lens(&self.buf[..8])?;
        let end = 8 + json_len + blob_len;
        if self.buf.len() < end {
            return Ok(None);
        }
        // Move the frame out rather than copying it: state blobs can be large.
        let rest = self.buf.split_off(end);
        let mut frame = std::mem::replace(&mut self.buf, rest);
        let msg = serde_json::from_slice(&frame[8..8 + json_len]).map_err(invalid)?;
        frame.drain(..8 + json_len);
        Ok(Some((msg, frame)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip_and_split_reads_reassemble() {
        let mut wire = Vec::new();
        write_frame(&mut wire, &Request::ParamValue(7), b"").unwrap();
        write_frame(&mut wire, &Request::LoadState, &[1, 2, 3]).unwrap();

        let (req, blob): (Request, _) = read_frame(&wire[..]).unwrap();
        assert!(matches!(req, Request::ParamValue(7)));
        assert!(blob.is_empty());

        // Feed the stream one byte at a time; the reader must only yield whole frames.
        struct Trickle<'a>(&'a [u8]);
        impl Read for Trickle<'_> {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                let Some((&b, rest)) = self.0.split_first() else {
                    return Err(io::ErrorKind::WouldBlock.into());
                };
                buf[0] = b;
                self.0 = rest;
                Ok(1)
            }
        }
        let mut reader = FrameReader::default();
        let mut src = Trickle(&wire);
        let mut got = Vec::new();
        while let Some((req, blob)) = reader.poll::<Request>(&mut src).unwrap() {
            got.push((req, blob));
        }
        assert_eq!(got.len(), 2);
        assert!(matches!(got[1].0, Request::LoadState));
        assert_eq!(got[1].1, [1, 2, 3]);
        assert!(reader.poll::<Request>(&mut src).unwrap().is_none());
    }

    #[test]
    fn oversized_frames_are_refused() {
        let mut wire = Vec::new();
        wire.extend_from_slice(&u32::MAX.to_le_bytes());
        wire.extend_from_slice(&0u32.to_le_bytes());
        assert!(read_frame::<Request>(&wire[..]).is_err());
    }

    #[test]
    fn a_closed_stream_is_an_error() {
        let mut reader = FrameReader::default();
        assert!(reader.poll::<Request>(&[][..]).is_err());
    }

    #[test]
    fn non_finite_param_info_crosses_the_wire() {
        let info = ParamInfo {
            id: 3,
            name: "odd".into(),
            min: f64::NEG_INFINITY,
            max: f64::INFINITY,
            default: f64::NAN,
            automatable: true,
        };
        let mut wire = Vec::new();
        let reply = Reply::Params(vec![WireParam::from(&info)]);
        write_frame(&mut wire, &reply, b"").unwrap();
        let (reply, _): (Reply, _) = read_frame(&wire[..]).unwrap();
        let Reply::Params(params) = reply else {
            panic!("{reply:?}");
        };
        let p = &params[0];
        assert_eq!((p.min, p.max, p.default), (f64::MIN, f64::MAX, 0.0));
    }
}
