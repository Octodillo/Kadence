//! Everything to do with audio files: decoding, resampling, writing, waveform
//! peak caches, disk streaming for playback and incremental recording.
//!
//! Audio in memory is planar `f32` ([`AudioData`]). Real-time entry points are
//! [`ClipReader::read`] and [`RecordInput::write`]; both are safe to call from
//! `Node::process` (no allocation, locking or I/O).

mod data;
mod decode;
mod error;
mod peaks;
mod record;
mod resample;
mod stream;
mod write;

pub use data::AudioData;
pub use decode::{AudioFileInfo, Decoder, decode_file, load_file};
pub use error::MediaError;
pub use peaks::{BASE_FRAMES_PER_PEAK, PeakBuilder, PeakCache, PeakLevel};
pub use record::{RecordInput, Recorder, RecordingStats};
pub use resample::{ChunkResampler, resample};
pub use stream::{ClipReader, DiskStreamer, ReadStatus, StreamerConfig};
pub use write::{FlacBits, WavFormat, write_flac, write_wav};

pub type Result<T, E = MediaError> = std::result::Result<T, E>;
