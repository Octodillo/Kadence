/// A fixed-length delay line used for latency compensation on one edge.
///
/// A ring buffer at least as long as the delay. It is usually exactly that
/// long; after [`adopt`](Self::adopt) it may be the longer buffer of the
/// line it replaced.
pub(crate) struct DelayLine {
    buf: Vec<f32>,
    /// The delay in samples; at most `buf.len()`.
    len: usize,
    /// Where the next sample is written.
    pos: usize,
}

/// The most audio [`DelayLine::adopt`] copies, in samples, when a line has
/// to grow past the buffer it takes over. It keeps a graph swap cheap on the
/// audio thread however long the lines are.
const MAX_CARRIED: usize = 8192;

impl DelayLine {
    pub fn new(len: usize) -> Self {
        debug_assert!(len > 0);
        Self {
            buf: vec![0.0; len],
            len,
            pos: 0,
        }
    }

    /// Adds `input` delayed by the line's length into `out`.
    pub fn mix_into(&mut self, input: &[f32], out: &mut [f32]) {
        let size = self.buf.len();
        // The sample written `len` ago; with a buffer as long as the delay
        // that is the one about to be overwritten.
        let mut read = (self.pos + size - self.len) % size;
        for (o, &x) in out.iter_mut().zip(input) {
            *o += self.buf[read];
            self.buf[self.pos] = x;
            read += 1;
            if read == size {
                read = 0;
            }
            self.pos += 1;
            if self.pos == size {
                self.pos = 0;
            }
        }
    }

    /// Takes over the audio in flight in `old`, the line on the same edge of
    /// the graph this one replaces, so a graph swap neither drops it nor
    /// plays a gap of silence in its place.
    ///
    /// When the delay fits `old`'s buffer (it always does unless a plugin's
    /// latency grew), the lines just trade buffers: a shorter delay skips
    /// the oldest audio and carries on. A delay that outgrew the buffer
    /// copies the newest [`MAX_CARRIED`] samples at most, so it plays
    /// silence for the extra length (and for what was not copied), then
    /// what it held. RT-safe: nothing is allocated and the copy is bounded.
    pub fn adopt(&mut self, old: &mut DelayLine) {
        if self.len <= old.buf.len() {
            std::mem::swap(&mut self.buf, &mut old.buf);
            self.pos = old.pos;
            return;
        }
        // The newest samples go last in this (longer, still silent) buffer,
        // which is read from `pos` onwards. `old` holds them just before
        // its own `pos`, wrapping.
        let keep = old.buf.len().min(MAX_CARRIED);
        let start = self.buf.len() - keep;
        let wrapped = keep.saturating_sub(old.pos);
        let (first, second) = self.buf[start..].split_at_mut(wrapped);
        first.copy_from_slice(&old.buf[old.buf.len() - wrapped..]);
        second.copy_from_slice(&old.buf[old.pos - (keep - wrapped)..old.pos]);
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
        // It runs in the longer buffer now, for as long as needed.
        let more: Vec<f32> = (11..=30).map(|x| x as f32).collect();
        let expected: Vec<f32> = (9..=28).map(|x| x as f32).collect();
        assert_eq!(run(&mut new, &more), expected);

        // Growing back within that buffer needs no copy either, and finds
        // the older audio still there.
        let mut longer = DelayLine::new(4);
        longer.adopt(&mut new);
        assert_eq!(run(&mut longer, &[31.0, 32.0]), [27.0, 28.0]);
    }

    #[test]
    fn a_line_that_outgrows_a_long_buffer_carries_a_bounded_tail() {
        let len = MAX_CARRIED + 100;
        let mut old = primed(len, len + 7);
        let mut new = DelayLine::new(len + 50);
        new.adopt(&mut old);
        let out = run(&mut new, &vec![0.0; len + 50]);
        // Silence for the extra length and for what was not carried, then
        // the newest samples in order, ending on the last one fed.
        let (silent, carried) = out.split_at(50 + 100);
        assert!(silent.iter().all(|&x| x == 0.0));
        let expected: Vec<f32> = (0..MAX_CARRIED).map(|i| (107 + 1 + i) as f32).collect();
        assert_eq!(carried, expected);
    }
}
