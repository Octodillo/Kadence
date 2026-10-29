use rubato::audioadapter_buffers::direct::SequentialSliceOfVecs;
use rubato::{Fft, FixedSync, Resampler};

use crate::{AudioData, MediaError, Result};

const CHUNK: usize = 1024;

fn err(e: impl std::fmt::Display) -> MediaError {
    MediaError::Resample(e.to_string())
}

/// Resample a whole clip in memory to `sample_rate` (FFT-based, fixed ratio).
pub fn resample(data: &AudioData, sample_rate: u32) -> Result<AudioData> {
    let ch = data.num_channels();
    if data.sample_rate == sample_rate || ch == 0 {
        return Ok(AudioData {
            sample_rate,
            channels: data.channels.clone(),
        });
    }
    let frames = data.frames();
    let mut rs = Fft::<f32>::new(
        data.sample_rate as usize,
        sample_rate as usize,
        CHUNK,
        ch,
        FixedSync::Both,
    )
    .map_err(err)?;
    let out_len = rs.process_all_needed_output_len(frames);
    let mut out = vec![vec![0.0f32; out_len]; ch];
    let input = SequentialSliceOfVecs::new(&data.channels, ch, frames).map_err(err)?;
    let mut output = SequentialSliceOfVecs::new_mut(&mut out, ch, out_len).map_err(err)?;
    let (_, produced) = rs
        .process_all_into_buffer(&input, &mut output, frames, None)
        .map_err(err)?;
    for plane in &mut out {
        plane.truncate(produced);
    }
    Ok(AudioData {
        sample_rate,
        channels: out,
    })
}

/// Incremental resampler for streams: push interleaved input of any length, get
/// interleaved output appended as it becomes available. The startup delay is trimmed,
/// so output frame `n` lines up with input time `n / ratio`.
///
/// Not real-time safe (output is appended to a `Vec`); the disk streamer runs it on its
/// fill thread.
pub struct ChunkResampler {
    rs: Fft<f32>,
    channels: usize,
    input: Vec<Vec<f32>>,
    filled: usize,
    output: Vec<Vec<f32>>,
    to_trim: usize,
    /// Input frames pushed since the last reset, used to size the flush.
    pushed: u64,
    produced: u64,
    ratio: f64,
}

impl ChunkResampler {
    pub fn new(from: u32, to: u32, channels: usize) -> Result<Self> {
        let rs = Fft::<f32>::new(
            from as usize,
            to as usize,
            CHUNK,
            channels,
            FixedSync::Input,
        )
        .map_err(err)?;
        let input = vec![vec![0.0; rs.input_frames_max()]; channels];
        let output = vec![vec![0.0; rs.output_frames_max()]; channels];
        let to_trim = rs.output_delay();
        Ok(Self {
            rs,
            channels,
            input,
            filled: 0,
            output,
            to_trim,
            pushed: 0,
            produced: 0,
            ratio: f64::from(to) / f64::from(from),
        })
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Drop all internal state, as after a seek.
    pub fn reset(&mut self) {
        self.rs.reset();
        self.filled = 0;
        self.to_trim = self.rs.output_delay();
        self.pushed = 0;
        self.produced = 0;
    }

    /// Feed interleaved input; resampled interleaved output is appended to `out`.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) -> Result<()> {
        let ch = self.channels;
        let mut frames = input.chunks_exact(ch);
        loop {
            let need = self.rs.input_frames_next();
            while self.filled < need {
                let Some(frame) = frames.next() else {
                    return Ok(());
                };
                for (plane, s) in self.input.iter_mut().zip(frame) {
                    plane[self.filled] = *s;
                }
                self.filled += 1;
                self.pushed += 1;
            }
            self.run(None, out)?;
        }
    }

    /// Push out what is still inside the resampler (end of input).
    pub fn flush(&mut self, out: &mut Vec<f32>) -> Result<()> {
        let expected = (self.pushed as f64 * self.ratio).ceil() as u64;
        let mut guard = 0;
        while self.produced < expected && guard < 64 {
            let partial = self.filled;
            self.run(Some(partial), out)?;
            guard += 1;
        }
        let excess = (self.produced.saturating_sub(expected)) as usize * self.channels;
        out.truncate(out.len().saturating_sub(excess));
        self.produced = self.produced.min(expected);
        Ok(())
    }

    fn run(&mut self, partial: Option<usize>, out: &mut Vec<f32>) -> Result<()> {
        let ch = self.channels;
        let in_len = self.input[0].len();
        let out_len = self.output[0].len();
        let input = SequentialSliceOfVecs::new(&self.input, ch, in_len).map_err(err)?;
        let mut output =
            SequentialSliceOfVecs::new_mut(&mut self.output, ch, out_len).map_err(err)?;
        let indexing = rubato::Indexing {
            input_offset: 0,
            output_offset: 0,
            partial_len: partial,
            active_channels_mask: None,
        };
        let (_, n_out) = self
            .rs
            .process_into_buffer(&input, &mut output, Some(&indexing))
            .map_err(err)?;
        self.filled = 0;
        let skip = self.to_trim.min(n_out);
        self.to_trim -= skip;
        out.reserve((n_out - skip) * ch);
        for f in skip..n_out {
            for plane in &self.output {
                out.push(plane[f]);
            }
        }
        self.produced += (n_out - skip) as u64;
        Ok(())
    }
}
