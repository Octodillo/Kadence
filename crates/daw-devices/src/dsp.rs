//! DSP building blocks shared by the devices. Everything here is real-time
//! safe once constructed.

use std::f64::consts::PI;

/// Decibel values at or below this are treated as silence.
pub const MIN_DB: f64 = -70.0;

pub fn db_to_gain(db: f64) -> f64 {
    if db <= MIN_DB {
        0.0
    } else {
        10f64.powf(db / 20.0)
    }
}

pub fn gain_to_db(gain: f64) -> f64 {
    if gain <= 0.0 {
        MIN_DB
    } else {
        (20.0 * gain.log10()).max(MIN_DB)
    }
}

/// Flush values too small to hear to zero, so recursive state never decays
/// into subnormal numbers (which are very slow on most CPUs). NaN and
/// infinity are zeroed too, so one bad input sample can't poison the state.
#[inline]
pub fn flush(x: f32) -> f32 {
    if (1e-15..f32::INFINITY).contains(&x.abs()) {
        x
    } else {
        0.0
    }
}

#[inline]
pub fn flush64(x: f64) -> f64 {
    if (1e-15..f64::INFINITY).contains(&x.abs()) {
        x
    } else {
        0.0
    }
}

/// Coefficient for a one-pole filter that moves ~63% of the way in `ms`.
pub fn one_pole_coef(ms: f64, sample_rate: f64) -> f32 {
    if ms <= 0.0 || sample_rate <= 0.0 {
        return 1.0;
    }
    (1.0 - (-1000.0 / (ms * sample_rate)).exp()) as f32
}

/// One-pole parameter smoother, used to avoid zipper noise.
#[derive(Clone, Copy, Debug)]
pub struct Smoother {
    current: f32,
    target: f32,
    coef: f32,
}

impl Smoother {
    pub fn new(value: f32) -> Self {
        Self {
            current: value,
            target: value,
            coef: 1.0,
        }
    }

    pub fn set_time(&mut self, ms: f64, sample_rate: f64) {
        self.coef = one_pole_coef(ms, sample_rate);
    }

    pub fn set_target(&mut self, value: f32) {
        self.target = value;
    }

    pub fn reset(&mut self, value: f32) {
        self.current = value;
        self.target = value;
    }

    pub fn target(&self) -> f32 {
        self.target
    }

    #[inline]
    pub fn tick(&mut self) -> f32 {
        let next = self.current + (self.target - self.current) * self.coef;
        // Snap once a step no longer moves the value (or is inaudibly small),
        // so large values like delay times don't stall short of the target.
        if next == self.current || (self.target - next).abs() < 1e-6 {
            self.current = self.target;
        } else {
            self.current = next;
        }
        self.current
    }
}

/// Normalized biquad coefficients (a0 = 1), designed with the RBJ cookbook.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BiquadCoefs {
    pub b0: f64,
    pub b1: f64,
    pub b2: f64,
    pub a1: f64,
    pub a2: f64,
}

impl Default for BiquadCoefs {
    fn default() -> Self {
        Self::IDENTITY
    }
}

struct Rbj {
    a: f64,
    cos: f64,
    alpha: f64,
}

fn rbj(sample_rate: f64, freq: f64, q: f64, gain_db: f64) -> Rbj {
    let f = freq.clamp(1.0, sample_rate * 0.49);
    let w0 = 2.0 * PI * f / sample_rate;
    Rbj {
        a: 10f64.powf(gain_db / 40.0),
        cos: w0.cos(),
        alpha: w0.sin() / (2.0 * q.max(1e-3)),
    }
}

impl BiquadCoefs {
    pub const IDENTITY: Self = Self {
        b0: 1.0,
        b1: 0.0,
        b2: 0.0,
        a1: 0.0,
        a2: 0.0,
    };

    fn normalize(b0: f64, b1: f64, b2: f64, a0: f64, a1: f64, a2: f64) -> Self {
        Self {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
        }
    }

    pub fn peaking(sample_rate: f64, freq: f64, q: f64, gain_db: f64) -> Self {
        let Rbj { a, cos, alpha } = rbj(sample_rate, freq, q, gain_db);
        Self::normalize(
            1.0 + alpha * a,
            -2.0 * cos,
            1.0 - alpha * a,
            1.0 + alpha / a,
            -2.0 * cos,
            1.0 - alpha / a,
        )
    }

    pub fn low_shelf(sample_rate: f64, freq: f64, q: f64, gain_db: f64) -> Self {
        let Rbj { a, cos, alpha } = rbj(sample_rate, freq, q, gain_db);
        let s = 2.0 * a.sqrt() * alpha;
        Self::normalize(
            a * ((a + 1.0) - (a - 1.0) * cos + s),
            2.0 * a * ((a - 1.0) - (a + 1.0) * cos),
            a * ((a + 1.0) - (a - 1.0) * cos - s),
            (a + 1.0) + (a - 1.0) * cos + s,
            -2.0 * ((a - 1.0) + (a + 1.0) * cos),
            (a + 1.0) + (a - 1.0) * cos - s,
        )
    }

    pub fn high_shelf(sample_rate: f64, freq: f64, q: f64, gain_db: f64) -> Self {
        let Rbj { a, cos, alpha } = rbj(sample_rate, freq, q, gain_db);
        let s = 2.0 * a.sqrt() * alpha;
        Self::normalize(
            a * ((a + 1.0) + (a - 1.0) * cos + s),
            -2.0 * a * ((a - 1.0) + (a + 1.0) * cos),
            a * ((a + 1.0) + (a - 1.0) * cos - s),
            (a + 1.0) - (a - 1.0) * cos + s,
            2.0 * ((a - 1.0) - (a + 1.0) * cos),
            (a + 1.0) - (a - 1.0) * cos - s,
        )
    }

    pub fn low_pass(sample_rate: f64, freq: f64, q: f64) -> Self {
        let Rbj { cos, alpha, .. } = rbj(sample_rate, freq, q, 0.0);
        let b = (1.0 - cos) / 2.0;
        Self::normalize(b, 2.0 * b, b, 1.0 + alpha, -2.0 * cos, 1.0 - alpha)
    }

    pub fn high_pass(sample_rate: f64, freq: f64, q: f64) -> Self {
        let Rbj { cos, alpha, .. } = rbj(sample_rate, freq, q, 0.0);
        let b = (1.0 + cos) / 2.0;
        Self::normalize(b, -2.0 * b, b, 1.0 + alpha, -2.0 * cos, 1.0 - alpha)
    }

    /// Magnitude response at `freq`, as a linear gain.
    pub fn magnitude(&self, sample_rate: f64, freq: f64) -> f64 {
        let w = 2.0 * PI * freq / sample_rate;
        let (c1, s1) = (w.cos(), w.sin());
        let (c2, s2) = ((2.0 * w).cos(), (2.0 * w).sin());
        let nr = self.b0 + self.b1 * c1 + self.b2 * c2;
        let ni = -(self.b1 * s1 + self.b2 * s2);
        let dr = 1.0 + self.a1 * c1 + self.a2 * c2;
        let di = -(self.a1 * s1 + self.a2 * s2);
        ((nr * nr + ni * ni) / (dr * dr + di * di)).sqrt()
    }
}

/// Transposed direct form II biquad state.
#[derive(Clone, Copy, Debug, Default)]
pub struct Biquad {
    s1: f64,
    s2: f64,
}

impl Biquad {
    #[inline]
    pub fn process(&mut self, c: &BiquadCoefs, x: f32) -> f32 {
        let x = x as f64;
        let y = c.b0 * x + self.s1;
        self.s1 = c.b1 * x - c.a1 * y + self.s2;
        self.s2 = c.b2 * x - c.a2 * y;
        // Flush both states together: zeroing just one injects an error that
        // a high-Q, low-frequency filter amplifies into a limit cycle. A
        // non-finite state would never recover, so it resets the filter.
        if (flush64(self.s1) == 0.0 && flush64(self.s2) == 0.0)
            || !(self.s1.is_finite() && self.s2.is_finite())
        {
            self.s1 = 0.0;
            self.s2 = 0.0;
        }
        y as f32
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

/// Topology-preserving state-variable filter (Simper/Cytomic), which stays
/// stable under fast cutoff modulation.
#[derive(Clone, Copy, Debug, Default)]
pub struct Svf {
    ic1: f32,
    ic2: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SvfMode {
    LowPass,
    HighPass,
    BandPass,
}

impl Svf {
    /// `resonance` in `0..=1`; 1 is just short of self-oscillation.
    #[inline]
    pub fn process(
        &mut self,
        x: f32,
        cutoff: f32,
        resonance: f32,
        sample_rate: f32,
        mode: SvfMode,
    ) -> f32 {
        let fc = cutoff.clamp(10.0, sample_rate * 0.45);
        let g = (std::f32::consts::PI * fc / sample_rate).tan();
        let k = 2.0 - 1.98 * resonance.clamp(0.0, 1.0);
        let a1 = 1.0 / (1.0 + g * (g + k));
        let a2 = g * a1;
        let a3 = g * a2;
        let v3 = x - self.ic2;
        let v1 = a1 * self.ic1 + a2 * v3;
        let v2 = self.ic2 + a2 * self.ic1 + a3 * v3;
        self.ic1 = flush(2.0 * v1 - self.ic1);
        self.ic2 = flush(2.0 * v2 - self.ic2);
        match mode {
            SvfMode::LowPass => v2,
            SvfMode::BandPass => v1,
            SvfMode::HighPass => x - k * v1 - v2,
        }
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

/// One-pole low-pass, used for damping in feedback paths.
#[derive(Clone, Copy, Debug, Default)]
pub struct OnePole {
    z: f32,
}

impl OnePole {
    /// `coef` is the amount of new input per sample, in `0..=1`.
    #[inline]
    pub fn process(&mut self, x: f32, coef: f32) -> f32 {
        self.z = flush(self.z + (x - self.z) * coef);
        self.z
    }

    pub fn reset(&mut self) {
        self.z = 0.0;
    }
}

/// Coefficient for [`OnePole`] with a -3 dB point near `freq`.
pub fn one_pole_lowpass_coef(freq: f64, sample_rate: f64) -> f32 {
    (1.0 - (-2.0 * PI * freq.min(sample_rate * 0.49) / sample_rate).exp()) as f32
}

/// Circular delay line with fractional reads.
#[derive(Clone, Debug, Default)]
pub struct DelayLine {
    buf: Vec<f32>,
    mask: usize,
    write: usize,
}

impl DelayLine {
    /// Allocates; call from `prepare`.
    pub fn new(max_delay: usize) -> Self {
        let len = (max_delay + 2).next_power_of_two();
        Self {
            buf: vec![0.0; len],
            mask: len - 1,
            write: 0,
        }
    }

    /// Longest delay that can be read, in samples.
    pub fn max_delay(&self) -> usize {
        self.buf.len().saturating_sub(2)
    }

    #[inline]
    pub fn push(&mut self, x: f32) {
        if self.buf.is_empty() {
            return;
        }
        self.write = (self.write + 1) & self.mask;
        self.buf[self.write] = flush(x);
    }

    /// Sample written `delay` pushes ago (0 = the latest), linearly interpolated.
    #[inline]
    pub fn read(&self, delay: f32) -> f32 {
        if self.buf.is_empty() {
            return 0.0;
        }
        let d = delay.clamp(0.0, self.max_delay() as f32);
        let i = d as usize;
        let frac = d - i as f32;
        let a = self.buf[(self.write.wrapping_sub(i)) & self.mask];
        let b = self.buf[(self.write.wrapping_sub(i + 1)) & self.mask];
        a + (b - a) * frac
    }

    pub fn clear(&mut self) {
        self.buf.fill(0.0);
    }
}

/// ADSR envelope: linear attack, exponential decay and release.
#[derive(Clone, Copy, Debug)]
pub struct Adsr {
    stage: Stage,
    level: f32,
    attack_step: f32,
    decay_coef: f32,
    sustain: f32,
    release_coef: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    Idle,
    Attack,
    Decay,
    Sustain,
    Release,
}

/// Level below which a releasing envelope is considered finished (-80 dB).
const ENV_FLOOR: f32 = 1e-4;

fn exp_coef(ms: f64, sample_rate: f64) -> f32 {
    // Reaches -60 dB of the remaining distance after `ms`.
    let n = (ms.max(0.1) / 1000.0 * sample_rate).max(1.0);
    (-6.907_755 / n).exp() as f32
}

impl Default for Adsr {
    fn default() -> Self {
        Self {
            stage: Stage::Idle,
            level: 0.0,
            attack_step: 1.0,
            decay_coef: 0.0,
            sustain: 1.0,
            release_coef: 0.0,
        }
    }
}

impl Adsr {
    pub fn set(
        &mut self,
        attack_ms: f64,
        decay_ms: f64,
        sustain: f64,
        release_ms: f64,
        sample_rate: f64,
    ) {
        let attack_samples = attack_ms / 1000.0 * sample_rate;
        self.attack_step = if attack_samples < 1.0 {
            1.0
        } else {
            (1.0 / attack_samples) as f32
        };
        self.decay_coef = exp_coef(decay_ms, sample_rate);
        self.sustain = sustain.clamp(0.0, 1.0) as f32;
        self.release_coef = exp_coef(release_ms, sample_rate);
    }

    /// Start (or restart from the current level, so retriggering doesn't click).
    pub fn gate_on(&mut self) {
        self.stage = Stage::Attack;
    }

    pub fn gate_off(&mut self) {
        if self.stage != Stage::Idle {
            self.stage = Stage::Release;
        }
    }

    pub fn kill(&mut self) {
        self.stage = Stage::Idle;
        self.level = 0.0;
    }

    pub fn is_active(&self) -> bool {
        self.stage != Stage::Idle
    }

    pub fn is_releasing(&self) -> bool {
        self.stage == Stage::Release
    }

    pub fn level(&self) -> f32 {
        self.level
    }

    #[inline]
    pub fn tick(&mut self) -> f32 {
        match self.stage {
            Stage::Idle => {}
            Stage::Attack => {
                self.level += self.attack_step;
                if self.level >= 1.0 {
                    self.level = 1.0;
                    self.stage = Stage::Decay;
                }
            }
            Stage::Decay => {
                self.level = self.sustain + (self.level - self.sustain) * self.decay_coef;
                if self.level - self.sustain < ENV_FLOOR {
                    self.level = self.sustain;
                    self.stage = Stage::Sustain;
                }
            }
            Stage::Sustain => {
                self.level = self.sustain;
                if self.sustain <= 0.0 {
                    self.stage = Stage::Idle;
                }
            }
            Stage::Release => {
                self.level *= self.release_coef;
                if self.level < ENV_FLOOR {
                    self.kill();
                }
            }
        }
        self.level
    }
}

/// PolyBLEP correction for a discontinuity at phase 0.
#[inline]
pub fn poly_blep(t: f32, dt: f32) -> f32 {
    if t < dt {
        let t = t / dt;
        t + t - t * t - 1.0
    } else if t > 1.0 - dt {
        let t = (t - 1.0) / dt;
        t * t + t + t + 1.0
    } else {
        0.0
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Waveform {
    Saw,
    Square,
    Triangle,
    Sine,
}

impl Waveform {
    pub const LABELS: &'static [&'static str] = &["Saw", "Square", "Triangle", "Sine"];

    pub fn from_index(i: usize) -> Self {
        match i {
            0 => Self::Saw,
            1 => Self::Square,
            2 => Self::Triangle,
            _ => Self::Sine,
        }
    }

    /// Band-limited (PolyBLEP) sample at phase `t` in `0..1` with phase step `dt`.
    #[inline]
    pub fn sample(self, t: f32, dt: f32) -> f32 {
        match self {
            Self::Saw => 2.0 * t - 1.0 - poly_blep(t, dt),
            Self::Square => {
                let naive = if t < 0.5 { 1.0 } else { -1.0 };
                let t2 = if t < 0.5 { t + 0.5 } else { t - 0.5 };
                naive + poly_blep(t, dt) - poly_blep(t2, dt)
            }
            Self::Triangle => 1.0 - 4.0 * (t - 0.5).abs(),
            Self::Sine => (std::f32::consts::TAU * t).sin(),
        }
    }
}

/// Frequency in Hz of a (possibly fractional) MIDI key.
#[inline]
pub fn key_to_hz(key: f32) -> f32 {
    440.0 * ((key - 69.0) / 12.0).exp2()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn db_conversions() {
        assert_eq!(db_to_gain(0.0), 1.0);
        assert!((db_to_gain(-6.0) - 0.501_187).abs() < 1e-5);
        assert_eq!(db_to_gain(MIN_DB), 0.0);
        assert!((gain_to_db(0.5) + 6.0206).abs() < 1e-3);
    }

    #[test]
    fn biquad_designs_hit_their_targets() {
        let sr = 48000.0;
        let p = BiquadCoefs::peaking(sr, 1000.0, 1.0, 6.0);
        assert!((gain_to_db(p.magnitude(sr, 1000.0)) - 6.0).abs() < 1e-6);
        let ls = BiquadCoefs::low_shelf(sr, 200.0, 0.707, -12.0);
        assert!((gain_to_db(ls.magnitude(sr, 10.0)) + 12.0).abs() < 0.1);
        assert!(gain_to_db(ls.magnitude(sr, 10000.0)).abs() < 0.1);
        let hs = BiquadCoefs::high_shelf(sr, 5000.0, 0.707, 9.0);
        assert!((gain_to_db(hs.magnitude(sr, 20000.0)) - 9.0).abs() < 0.2);
        let lp = BiquadCoefs::low_pass(sr, 1000.0, std::f64::consts::FRAC_1_SQRT_2);
        assert!((gain_to_db(lp.magnitude(sr, 1000.0)) + 3.01).abs() < 0.05);
        let hp = BiquadCoefs::high_pass(sr, 1000.0, std::f64::consts::FRAC_1_SQRT_2);
        assert!((gain_to_db(hp.magnitude(sr, 1000.0)) + 3.01).abs() < 0.05);
    }

    #[test]
    fn delay_line_reads_back() {
        let mut d = DelayLine::new(100);
        for i in 0..10 {
            d.push(i as f32);
        }
        assert_eq!(d.read(0.0), 9.0);
        assert_eq!(d.read(3.0), 6.0);
        assert_eq!(d.read(3.5), 5.5);
    }

    #[test]
    fn adsr_shape() {
        let mut e = Adsr::default();
        e.set(1.0, 10.0, 0.5, 10.0, 1000.0);
        e.gate_on();
        assert_eq!(e.tick(), 1.0);
        for _ in 0..50 {
            e.tick();
        }
        assert_eq!(e.level(), 0.5);
        e.gate_off();
        for _ in 0..50 {
            e.tick();
        }
        assert!(!e.is_active());
        assert_eq!(e.level(), 0.0);
    }
}
