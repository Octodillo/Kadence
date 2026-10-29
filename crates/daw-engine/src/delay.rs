/// A fixed-length delay line used for latency compensation on one edge.
pub(crate) struct DelayLine {
    buf: Vec<f32>,
    pos: usize,
}

impl DelayLine {
    pub fn new(len: usize) -> Self {
        debug_assert!(len > 0);
        Self {
            buf: vec![0.0; len],
            pos: 0,
        }
    }

    /// Adds `input` delayed by the line's length into `out`.
    pub fn mix_into(&mut self, input: &[f32], out: &mut [f32]) {
        let len = self.buf.len();
        for (o, &x) in out.iter_mut().zip(input) {
            *o += self.buf[self.pos];
            self.buf[self.pos] = x;
            self.pos += 1;
            if self.pos == len {
                self.pos = 0;
            }
        }
    }

    pub fn reset(&mut self) {
        self.buf.fill(0.0);
        self.pos = 0;
    }
}
