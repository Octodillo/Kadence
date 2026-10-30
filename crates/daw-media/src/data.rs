/// Decoded audio held in memory: one `Vec<f32>` per channel, all the same length.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AudioData {
    pub sample_rate: u32,
    pub channels: Vec<Vec<f32>>,
}

impl AudioData {
    /// Silent audio with `channels` channels of `frames` frames.
    pub fn silence(sample_rate: u32, channels: usize, frames: usize) -> Self {
        Self {
            sample_rate,
            channels: vec![vec![0.0; frames]; channels],
        }
    }

    pub fn num_channels(&self) -> usize {
        self.channels.len()
    }

    pub fn frames(&self) -> usize {
        self.channels.first().map_or(0, Vec::len)
    }

    /// Duration in seconds.
    pub fn duration(&self) -> f64 {
        if self.sample_rate == 0 {
            0.0
        } else {
            self.frames() as f64 / f64::from(self.sample_rate)
        }
    }

    /// Interleave into a single buffer (`L R L R ...`).
    pub fn interleaved(&self) -> Vec<f32> {
        let ch = self.num_channels();
        let mut out = vec![0.0; ch * self.frames()];
        for (c, plane) in self.channels.iter().enumerate() {
            for (i, s) in plane.iter().enumerate() {
                out[i * ch + c] = *s;
            }
        }
        out
    }

    /// Build from an interleaved buffer. Trailing samples that don't fill a frame are dropped.
    pub fn from_interleaved(sample_rate: u32, channels: usize, samples: &[f32]) -> Self {
        let channels = channels.max(1);
        let frames = samples.len() / channels;
        // Not `vec![Vec::with_capacity(..); n]`: cloning drops the capacity.
        let mut data = Self {
            sample_rate,
            channels: (0..channels).map(|_| Vec::with_capacity(frames)).collect(),
        };
        data.extend_interleaved(samples);
        data
    }

    /// Append interleaved frames in one pass. Trailing samples that don't fill a frame
    /// are dropped.
    pub fn extend_interleaved(&mut self, samples: &[f32]) {
        let ch = self.channels.len();
        if ch == 0 {
            return;
        }
        if ch == 1 {
            self.channels[0].extend_from_slice(samples);
            return;
        }
        for plane in &mut self.channels {
            plane.reserve(samples.len() / ch);
        }
        for frame in samples.chunks_exact(ch) {
            for (plane, s) in self.channels.iter_mut().zip(frame) {
                plane.push(*s);
            }
        }
    }
}
