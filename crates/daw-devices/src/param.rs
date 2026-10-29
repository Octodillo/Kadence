//! Parameter metadata and storage.

use daw_core::event::ParamId;
use daw_core::plugin::ParamInfo;

/// How a parameter's values are interpreted.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum ParamKind {
    Continuous,
    /// Whole numbers between `min` and `max`.
    Integer,
    /// `0.0` is off, `1.0` is on.
    Toggle,
    /// Index into the given labels.
    Choice(&'static [&'static str]),
}

/// Unit used for display.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ParamUnit {
    None,
    Decibels,
    Hertz,
    Milliseconds,
    Percent,
    Semitones,
    Cents,
    Octaves,
    Ratio,
    Pan,
}

/// How a knob or slider maps its travel to the value range.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Taper {
    Linear,
    /// Logarithmic; `min` must be positive.
    Log,
}

/// Full description of one parameter.
#[derive(Copy, Clone, Debug)]
pub struct ParamSpec {
    pub id: ParamId,
    /// Stable key used in saved state.
    pub key: &'static str,
    pub name: &'static str,
    pub min: f64,
    pub max: f64,
    pub default: f64,
    pub kind: ParamKind,
    pub unit: ParamUnit,
    pub taper: Taper,
    pub automatable: bool,
}

impl ParamSpec {
    pub const fn new(
        id: ParamId,
        key: &'static str,
        name: &'static str,
        min: f64,
        max: f64,
        default: f64,
        unit: ParamUnit,
    ) -> Self {
        Self {
            id,
            key,
            name,
            min,
            max,
            default,
            kind: ParamKind::Continuous,
            unit,
            taper: Taper::Linear,
            automatable: true,
        }
    }

    pub const fn toggle(id: ParamId, key: &'static str, name: &'static str, default: bool) -> Self {
        let mut s = Self::new(
            id,
            key,
            name,
            0.0,
            1.0,
            if default { 1.0 } else { 0.0 },
            ParamUnit::None,
        );
        s.kind = ParamKind::Toggle;
        s
    }

    pub const fn choice(
        id: ParamId,
        key: &'static str,
        name: &'static str,
        labels: &'static [&'static str],
        default: usize,
    ) -> Self {
        let mut s = Self::new(
            id,
            key,
            name,
            0.0,
            (labels.len() - 1) as f64,
            default as f64,
            ParamUnit::None,
        );
        s.kind = ParamKind::Choice(labels);
        s
    }

    pub const fn integer(self) -> Self {
        Self {
            kind: ParamKind::Integer,
            ..self
        }
    }

    pub const fn log(self) -> Self {
        Self {
            taper: Taper::Log,
            ..self
        }
    }

    pub fn info(&self) -> ParamInfo {
        ParamInfo {
            id: self.id,
            name: self.name.to_owned(),
            min: self.min,
            max: self.max,
            default: self.default,
            automatable: self.automatable,
        }
    }

    pub fn is_discrete(&self) -> bool {
        !matches!(self.kind, ParamKind::Continuous)
    }

    /// Clamp to the range and round discrete parameters. NaN becomes the default.
    pub fn sanitize(&self, value: f64) -> f64 {
        if value.is_nan() {
            return self.default;
        }
        let v = value.clamp(self.min, self.max);
        if self.is_discrete() { v.round() } else { v }
    }

    /// Map a plain value to `0..=1` along the taper.
    pub fn to_normalized(&self, value: f64) -> f64 {
        let v = self.sanitize(value);
        if self.max <= self.min {
            return 0.0;
        }
        match self.taper {
            Taper::Linear => (v - self.min) / (self.max - self.min),
            Taper::Log => (v / self.min).ln() / (self.max / self.min).ln(),
        }
    }

    /// Map `0..=1` along the taper to a plain value.
    pub fn from_normalized(&self, normalized: f64) -> f64 {
        let n = normalized.clamp(0.0, 1.0);
        let v = match self.taper {
            Taper::Linear => self.min + n * (self.max - self.min),
            Taper::Log => self.min * (self.max / self.min).powf(n),
        };
        self.sanitize(v)
    }

    /// Human-readable value with its unit, e.g. `-6.0 dB` or `Saw`.
    pub fn display(&self, value: f64) -> String {
        let v = self.sanitize(value);
        match self.kind {
            ParamKind::Toggle => return if v >= 0.5 { "On" } else { "Off" }.to_owned(),
            ParamKind::Choice(labels) => return labels[v as usize].to_owned(),
            ParamKind::Integer => return format!("{v:.0}"),
            ParamKind::Continuous => {}
        }
        match self.unit {
            ParamUnit::Decibels if v <= crate::dsp::MIN_DB => "-inf dB".to_owned(),
            ParamUnit::Decibels => format!("{v:.1} dB"),
            ParamUnit::Hertz if v >= 1000.0 => format!("{:.2} kHz", v / 1000.0),
            ParamUnit::Hertz => format!("{v:.0} Hz"),
            ParamUnit::Milliseconds if v >= 1000.0 => format!("{:.2} s", v / 1000.0),
            ParamUnit::Milliseconds => format!("{v:.1} ms"),
            ParamUnit::Percent => format!("{:.0} %", v * 100.0),
            ParamUnit::Semitones => format!("{v:+.0} st"),
            ParamUnit::Cents => format!("{v:+.0} ct"),
            ParamUnit::Octaves => format!("{v:+.2} oct"),
            ParamUnit::Ratio => format!("{v:.1}:1"),
            ParamUnit::Pan if v.abs() < 0.005 => "C".to_owned(),
            ParamUnit::Pan if v < 0.0 => format!("{:.0} L", -v * 100.0),
            ParamUnit::Pan => format!("{:.0} R", v * 100.0),
            ParamUnit::None => format!("{v:.2}"),
        }
    }
}

/// Current values for a device's parameters. Never allocates after `new`.
#[derive(Debug)]
pub struct ParamStore {
    specs: &'static [ParamSpec],
    values: Box<[f64]>,
}

impl ParamStore {
    pub fn new(specs: &'static [ParamSpec]) -> Self {
        debug_assert!(specs.iter().enumerate().all(|(i, s)| s.id as usize == i));
        Self {
            specs,
            values: specs.iter().map(|s| s.default).collect(),
        }
    }

    pub fn get(&self, id: ParamId) -> f64 {
        self.values.get(id as usize).copied().unwrap_or(0.0)
    }

    pub fn get_f32(&self, id: ParamId) -> f32 {
        self.get(id) as f32
    }

    pub fn get_bool(&self, id: ParamId) -> bool {
        self.get(id) >= 0.5
    }

    pub fn get_index(&self, id: ParamId) -> usize {
        self.get(id) as usize
    }

    /// Returns false for an unknown id.
    pub fn set(&mut self, id: ParamId, value: f64) -> bool {
        match (
            self.specs.get(id as usize),
            self.values.get_mut(id as usize),
        ) {
            (Some(spec), Some(slot)) => {
                *slot = spec.sanitize(value);
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOG: ParamSpec =
        ParamSpec::new(0, "f", "Freq", 20.0, 20000.0, 1000.0, ParamUnit::Hertz).log();
    const CHOICE: ParamSpec = ParamSpec::choice(1, "w", "Wave", &["A", "B", "C"], 1);

    #[test]
    fn normalized_round_trip() {
        for v in [20.0, 100.0, 1000.0, 20000.0] {
            assert!((LOG.from_normalized(LOG.to_normalized(v)) - v).abs() < 1e-9);
        }
        assert!((LOG.to_normalized(632.455_532) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn sanitize_and_display() {
        assert_eq!(CHOICE.sanitize(1.7), 2.0);
        assert_eq!(CHOICE.sanitize(-3.0), 0.0);
        assert_eq!(CHOICE.display(1.0), "B");
        assert_eq!(LOG.display(1500.0), "1.50 kHz");
        assert_eq!(LOG.sanitize(f64::NAN), 1000.0);
    }
}
