//! What the Mix page needs, without naming Qt.
//!
//! [`MixerHandle`] is the mixer as the UI sees it: channel strips with
//! volume, pan, mute, solo, sends and an insert chain, plus their meters.
//! Like [`EngineHandle`](crate::engine::EngineHandle) it uses plain Rust
//! types so another front end can share it.
//!
//! The app's mixer is [`SessionMixer`](crate::session_mixer::SessionMixer):
//! the open project's tracks, edited through model commands (so they undo)
//! and metered from the running session.

/// A strip's track id, as a number.
pub type StripId = u128;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum StripKind {
    Track,
    Bus,
    Master,
}

#[derive(Clone, Debug, PartialEq)]
pub struct InsertInfo {
    /// The model device's id, for the device panel.
    pub uid: String,
    /// Built-in device id, such as `daw.compressor`.
    pub device_id: String,
    /// Switched on; off is bypassed.
    pub enabled: bool,
    pub name: String,
    pub has_gain_reduction: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SendInfo {
    pub target: StripId,
    pub target_name: String,
    pub level_db: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct StripInfo {
    pub id: StripId,
    pub name: String,
    pub kind: StripKind,
    /// Fader level; [`SILENT_DB`] or lower is silence.
    pub volume_db: f64,
    /// Balance, -1 (left) to 1 (right).
    pub pan: f64,
    pub muted: bool,
    pub soloed: bool,
    /// Silent because of mute or someone else's solo.
    pub silenced: bool,
    /// Whether the strip is an instrument track's, with an instrument slot.
    pub has_instrument: bool,
    /// The instrument in that slot; it is not among `inserts`.
    pub instrument: Option<InsertInfo>,
    /// The instrument's index among the app's instrument choices.
    pub instrument_choice: Option<usize>,
    /// The effects, after the instrument.
    pub inserts: Vec<InsertInfo>,
    pub sends: Vec<SendInfo>,
}

/// Meter readings since the last call to [`MixerHandle::levels`].
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct StripLevels {
    /// Linear peak per channel (mono strips repeat channel 0).
    pub peak: [f32; 2],
    /// Highest gain reduction among the strip's dynamics inserts, in dB.
    pub gain_reduction_db: Option<f32>,
}

/// A device the user can insert on a strip.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EffectChoice {
    pub id: String,
    pub name: String,
}

/// Fader positions at or below this are silence.
pub const SILENT_DB: f64 = -60.0;
/// Top of the fader.
pub const MAX_DB: f64 = 6.0;

/// Fader dB to linear gain.
pub fn db_to_gain(db: f64) -> f64 {
    if db <= SILENT_DB {
        0.0
    } else {
        daw_devices::dsp::db_to_gain(db.min(MAX_DB))
    }
}

/// What a meter shows: positions from 0 (-60 dB and below) to 1 (+6 dB).
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct MeterDisplay {
    pub level: [f64; 2],
    /// Recent highest level, held for a moment.
    pub hold: [f64; 2],
    /// Gain reduction in dB, falling back smoothly; `None` without a
    /// dynamics insert.
    pub gain_reduction_db: Option<f64>,
}

/// Meter ballistics: levels rise at once, fall at [`FALL_DB_PER_S`], and
/// peaks are held for [`HOLD_S`]. Runs on every UI tick, whether or not new
/// audio arrived, so meters fall back to rest when playback stops.
#[derive(Default)]
pub struct MeterBallistics {
    shown: Vec<MeterDisplay>,
    held_for: Vec<[f64; 2]>,
}

pub const FALL_DB_PER_S: f64 = 20.0;
pub const HOLD_S: f64 = 1.5;

/// A linear peak as a meter position.
pub fn meter_position(linear: f32) -> f64 {
    if linear <= 0.0 {
        return 0.0;
    }
    let db = 20.0 * f64::from(linear).log10();
    ((db - SILENT_DB) / (MAX_DB - SILENT_DB)).clamp(0.0, 1.0)
}

impl MeterBallistics {
    pub fn update(&mut self, levels: &[StripLevels], seconds: f64) -> &[MeterDisplay] {
        self.shown.resize(levels.len(), MeterDisplay::default());
        self.held_for.resize(levels.len(), [0.0; 2]);
        let fall = FALL_DB_PER_S * seconds.max(0.0);
        let fall_pos = fall / (MAX_DB - SILENT_DB);
        for ((shown, held_for), l) in self.shown.iter_mut().zip(&mut self.held_for).zip(levels) {
            for (ch, held) in held_for.iter_mut().enumerate() {
                let level = meter_position(l.peak[ch])
                    .max(shown.level[ch] - fall_pos)
                    .max(0.0);
                shown.level[ch] = level;
                *held += seconds.max(0.0);
                if level >= shown.hold[ch] || *held > HOLD_S {
                    shown.hold[ch] = level;
                    *held = 0.0;
                }
            }
            shown.gain_reduction_db = l.gain_reduction_db.map(|gr| {
                let prev = shown.gain_reduction_db.unwrap_or(0.0);
                f64::from(gr).max(prev - fall).max(0.0)
            });
        }
        &self.shown
    }
}

/// The mixer as the UI sees it. Called on the UI thread only.
pub trait MixerHandle {
    /// Strips in display order: tracks, then buses, then master.
    fn strips(&self) -> Vec<StripInfo>;
    fn set_volume(&mut self, id: StripId, db: f64);
    fn set_pan(&mut self, id: StripId, pan: f64);
    fn set_muted(&mut self, id: StripId, muted: bool);
    fn set_soloed(&mut self, id: StripId, soloed: bool);
    fn set_send_level(&mut self, id: StripId, target: StripId, db: f64);
    /// Appends a built-in effect to the strip's insert chain.
    fn add_insert(&mut self, id: StripId, device_id: &str) -> bool;
    /// Appends a plugin to the strip's insert chain, as a device called
    /// `name`.
    fn add_plugin_insert(
        &mut self,
        _id: StripId,
        _format: daw_core::plugin::PluginFormatKind,
        _plugin: &str,
        _name: &str,
    ) -> bool {
        false
    }
    /// Removes the effect at `index` in [`StripInfo::inserts`].
    fn remove_insert(&mut self, id: StripId, index: usize);
    /// Puts `choice` in an instrument track's instrument slot.
    fn set_instrument(&mut self, _id: StripId, _choice: &crate::instrument::InstrumentChoice) {}
    fn effects(&self) -> Vec<EffectChoice>;
    /// Meter readings, parallel to [`strips`](Self::strips). Resets them.
    fn levels(&mut self) -> Vec<StripLevels>;
    /// Called on the UI clock, `seconds` after the last call.
    fn tick(&mut self, _playing: bool, _seconds: f64) {}
    /// Changes whenever the strips may have changed from elsewhere (an
    /// edit in another view, undo, another project opened).
    fn revision(&self) -> u64 {
        0
    }
    /// Touch writing: while on, moving a control during playback records
    /// it as automation.
    fn set_write_automation(&mut self, _on: bool) {}
    fn write_automation(&self) -> bool {
        false
    }
    /// The user grabbed (`pressed`) or let go of a control.
    fn touch(&mut self, _id: StripId, _control: Control, _pressed: bool) {}
}

/// A strip control that can be automated.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Control {
    Volume,
    Pan,
    /// The send to this strip.
    Send(StripId),
    /// A parameter of a device on the strip (from the device panel).
    Param {
        device: daw_model::DeviceId,
        param: daw_core::event::ParamId,
    },
}

/// The mixer the app runs: the open project's, through the app state.
pub fn default_mixer() -> Box<dyn MixerHandle> {
    Box::new(crate::session_mixer::AppMixer)
}

/// Built-in devices that process their input: effects and utilities with
/// audio inputs, not instruments or generators like the metronome.
pub fn effect_choices() -> Vec<EffectChoice> {
    daw_devices::devices()
        .iter()
        .filter(|e| {
            e.info.category != daw_devices::DeviceCategory::Instrument
                && (e.create)().ports().audio_in > 0
        })
        .map(|e| EffectChoice {
            id: e.info.id.to_owned(),
            name: e.info.name.to_owned(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effects_list_only_has_effects() {
        let effects = effect_choices();
        assert!(effects.iter().any(|e| e.id == "daw.compressor"));
        assert!(effects.iter().all(|e| e.id != "daw.synth"));
        assert!(effects.iter().all(|e| e.id != "daw.metronome"));
        assert!(effects.iter().any(|e| e.id == "daw.utility"));
    }

    #[test]
    fn meters_fall_back_and_hold_peaks() {
        let mut b = MeterBallistics::default();
        let loud = StripLevels {
            peak: [1.0, 0.5],
            gain_reduction_db: Some(6.0),
        };
        let quiet = StripLevels {
            peak: [0.0, 0.0],
            gain_reduction_db: Some(0.0),
        };
        let first = b.update(&[loud], 0.03)[0];
        assert!((first.level[0] - meter_position(1.0)).abs() < 1e-9);
        let after = b.update(&[quiet], 1.0)[0];
        // 20 dB in a second, out of a 66 dB scale.
        assert!((first.level[0] - after.level[0] - 20.0 / 66.0).abs() < 1e-9);
        assert_eq!(after.hold, first.level, "peak held");
        assert_eq!(after.gain_reduction_db, Some(0.0));
        // Silence keeps falling to rest, and the hold drops after it expires.
        for _ in 0..10 {
            b.update(&[quiet], 1.0);
        }
        let rest = b.update(&[quiet], 0.03)[0];
        assert_eq!(rest.level, [0.0, 0.0]);
        assert_eq!(rest.hold, [0.0, 0.0]);
    }
}
