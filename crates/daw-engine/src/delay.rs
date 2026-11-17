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

    /// Takes over the audio in flight in `old`, the line on the same edge of
    /// the graph this one replaces, so a graph swap neither drops it nor
    /// plays a gap of silence in its place.
    ///
    /// Lines of the same length just trade buffers. When the length changed
    /// (a plugin's latency did), the newest samples are copied, as many as
    /// fit: a longer line plays its extra length as silence first, a shorter
    /// one skips the oldest audio. Either way the path stays continuous with
    /// what it carried before. RT-safe: the copy is bounded by the shorter
    /// line.
    pub fn adopt(&mut self, old: &mut DelayLine) {
        if self.buf.len() == old.buf.len() {
            std::mem::swap(self, old);
            return;
        }
        let keep = self.buf.len().min(old.buf.len());
        let extra = self.buf.len() - keep;
        let (silent, kept) = self.buf.split_at_mut(extra);
        silent.fill(0.0);
        // Oldest first: from the read position round to just before it.
        let oldest_first = old.buf[old.pos..].iter().chain(&old.buf[..old.pos]);
        for (dst, src) in kept.iter_mut().zip(oldest_first.skip(old.buf.len() - keep)) {
            *dst = *src;
        }
        self.pos = 0;
    }

    pub fn reset(&mut self) {
        self.buf.fill(0.0);
        self.pos = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feeds `input` through `line` and returns what comes out.
    fn run(line: &mut DelayLine, input: &[f32]) -> Vec<f32> {
        let mut out = vec![0.0; input.len()];
        line.mix_into(input, &mut out);
        out
    }

    /// A line of `len` that has carried 1, 2, .. `fed`.
    fn primed(len: usize, fed: usize) -> DelayLine {
        let mut line = DelayLine::new(len);
        let input: Vec<f32> = (1..=fed).map(|x| x as f32).collect();
        run(&mut line, &input);
        line
    }

    #[test]
    fn adopting_a_line_of_the_same_length_continues_it() {
        let mut old = primed(4, 6);
        let mut new = DelayLine::new(4);
        new.adopt(&mut old);
        assert_eq!(
            run(&mut new, &[7.0, 8.0, 9.0, 10.0, 11.0]),
            [3.0, 4.0, 5.0, 6.0, 7.0]
        );
        // The replaced line got the fresh buffer in exchange.
        assert_eq!(run(&mut old, &[1.0; 4]), [0.0; 4]);
    }

    #[test]
    fn a_longer_line_plays_its_extra_length_as_silence_first() {
        let mut old = primed(3, 5);
        let mut new = DelayLine::new(5);
        new.adopt(&mut old);
        assert_eq!(
            run(&mut new, &[6.0, 7.0, 8.0, 9.0, 10.0, 11.0]),
            [0.0, 0.0, 3.0, 4.0, 5.0, 6.0]
        );
    }

    #[test]
    fn a_shorter_line_keeps_the_newest_audio() {
        let mut old = primed(5, 7);
        let mut new = DelayLine::new(2);
        new.adopt(&mut old);
        assert_eq!(run(&mut new, &[8.0, 9.0, 10.0]), [6.0, 7.0, 8.0]);
    }
}
