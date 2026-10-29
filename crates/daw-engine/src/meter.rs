//! Output meters the RT thread fills and the UI reads.

use daw_core::meter::PeakMeter;

/// One peak meter per output channel of a node. Attach it with
/// [`GraphDesc::set_meter`](crate::GraphDesc::set_meter) and keep the `Arc`
/// to read it; the RT thread updates it after the node runs each block.
#[derive(Debug)]
pub struct NodeMeter {
    channels: Box<[PeakMeter]>,
}

impl NodeMeter {
    pub fn new(channels: usize) -> Self {
        Self {
            channels: (0..channels).map(|_| PeakMeter::new()).collect(),
        }
    }

    pub fn channels(&self) -> usize {
        self.channels.len()
    }

    pub fn channel(&self, ch: usize) -> Option<&PeakMeter> {
        self.channels.get(ch)
    }

    /// Reader side: the linear peak of each channel since the last call,
    /// resetting them.
    pub fn take(&self) -> Vec<f32> {
        self.channels.iter().map(PeakMeter::take).collect()
    }

    /// RT side.
    pub(crate) fn update(&self, ch: usize, samples: &[f32]) {
        if let Some(m) = self.channels.get(ch) {
            m.update(samples);
        }
    }
}
