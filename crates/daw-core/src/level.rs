//! Levels in decibels: the one place that turns dB into linear gain and back,
//! and the level constants every layer agrees on.
//!
//! [`db_to_gain`] and [`gain_to_db`] are the plain conversions. A control
//! whose bottom means "off" has a floor at or below which the gain is exactly
//! zero: [`db_to_gain_above`] and [`gain_to_db_above`] take that floor, and
//! a caller with a fixed one wraps them under a name that says which.
//!
//! All of these are plain arithmetic and real-time safe.

/// The level the project stores for silence: a fader, send or level lane
/// point pulled all the way down. A strip plays it, and anything lower, as a
/// gain of exactly zero.
pub const SILENCE_DB: f64 = -144.0;

/// The bottom of faders, meters and level lanes. A fader at or below it
/// stores [`SILENCE_DB`]; a ramp from silence plays from here.
pub const FADER_FLOOR_DB: f64 = -60.0;

/// The top of faders, meters and level lanes.
pub const FADER_MAX_DB: f64 = 6.0;

/// The height of a fader or meter in dB.
pub const FADER_SPAN_DB: f64 = FADER_MAX_DB - FADER_FLOOR_DB;

/// Linear gain for a level in dB, with no floor: only negative infinity
/// gives zero.
#[inline]
pub fn db_to_gain(db: f64) -> f64 {
    10f64.powf(db / 20.0)
}

/// [`db_to_gain`] in single precision, for per-sample work.
#[inline]
pub fn db_to_gain_f32(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

/// The level in dB of a linear gain, with no floor: zero gives negative
/// infinity, and a negative gain NaN.
#[inline]
pub fn gain_to_db(gain: f64) -> f64 {
    20.0 * gain.log10()
}

/// [`gain_to_db`] in single precision, for per-sample work.
#[inline]
pub fn gain_to_db_f32(gain: f32) -> f32 {
    20.0 * gain.log10()
}

/// Linear gain for a level in dB where `floor_db` and anything below it
/// (NaN included) is silence.
#[inline]
pub fn db_to_gain_above(db: f64, floor_db: f64) -> f64 {
    if db > floor_db { db_to_gain(db) } else { 0.0 }
}

/// The level in dB of a linear gain, never below `floor_db`: zero, negative
/// and NaN gains give the floor.
#[inline]
pub fn gain_to_db_above(gain: f64, floor_db: f64) -> f64 {
    if gain > 0.0 {
        gain_to_db(gain).max(floor_db)
    } else {
        floor_db
    }
}

/// A fader position in dB as the level the project stores: the fader's
/// bottom (and NaN) becomes [`SILENCE_DB`], and nothing goes above its top.
pub fn fader_to_stored_db(db: f64) -> f64 {
    if db > FADER_FLOOR_DB {
        db.min(FADER_MAX_DB)
    } else {
        SILENCE_DB
    }
}

/// Where a level in dB sits on a fader or meter, from 0 at
/// [`FADER_FLOOR_DB`] and below (and for NaN) to 1 at [`FADER_MAX_DB`] and
/// above.
pub fn fader_position(db: f64) -> f64 {
    if db > FADER_FLOOR_DB {
        ((db - FADER_FLOOR_DB) / FADER_SPAN_DB).min(1.0)
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_conversions_agree_with_each_other() {
        assert_eq!(db_to_gain(0.0), 1.0);
        assert!((db_to_gain(-6.0) - 0.501_187).abs() < 1e-5);
        assert!((gain_to_db(0.5) + 6.0206).abs() < 1e-3);
        assert_eq!(db_to_gain(f64::NEG_INFINITY), 0.0);
        assert_eq!(gain_to_db(0.0), f64::NEG_INFINITY);
        assert!(gain_to_db(-1.0).is_nan());
        for db in [-90.0, -12.5, 0.0, 6.0] {
            assert!((gain_to_db(db_to_gain(db)) - db).abs() < 1e-9);
            assert!((f64::from(db_to_gain_f32(db as f32)) - db_to_gain(db)).abs() < 1e-6);
            let gain = db_to_gain(db);
            assert!((f64::from(gain_to_db_f32(gain as f32)) - db).abs() < 1e-3);
        }
    }

    #[test]
    fn the_floor_and_everything_below_it_is_silence() {
        assert_eq!(db_to_gain_above(-70.0, -70.0), 0.0);
        assert_eq!(db_to_gain_above(-200.0, -70.0), 0.0);
        assert_eq!(db_to_gain_above(f64::NAN, -70.0), 0.0);
        assert_eq!(db_to_gain_above(-69.0, -70.0), db_to_gain(-69.0));
        assert_eq!(db_to_gain_above(f64::INFINITY, -70.0), f64::INFINITY);
    }

    #[test]
    fn a_gain_never_reads_below_the_floor() {
        assert_eq!(gain_to_db_above(0.0, -70.0), -70.0);
        assert_eq!(gain_to_db_above(-1.0, -70.0), -70.0);
        assert_eq!(gain_to_db_above(f64::NAN, -70.0), -70.0);
        assert_eq!(gain_to_db_above(1e-9, -70.0), -70.0);
        assert!((gain_to_db_above(0.5, -70.0) + 6.0206).abs() < 1e-3);
    }

    #[test]
    fn a_fader_at_its_bottom_stores_silence() {
        assert_eq!(fader_to_stored_db(FADER_FLOOR_DB), SILENCE_DB);
        assert_eq!(fader_to_stored_db(-80.0), SILENCE_DB);
        assert_eq!(fader_to_stored_db(-59.9), -59.9);
        assert_eq!(fader_to_stored_db(12.0), FADER_MAX_DB);
        assert_eq!(fader_to_stored_db(f64::NAN), SILENCE_DB);
    }

    #[test]
    fn fader_positions_run_from_the_floor_to_the_top() {
        assert_eq!(fader_position(SILENCE_DB), 0.0);
        assert_eq!(fader_position(FADER_FLOOR_DB), 0.0);
        assert_eq!(fader_position(FADER_MAX_DB), 1.0);
        assert_eq!(fader_position(20.0), 1.0);
        assert_eq!(fader_position(f64::NAN), 0.0);
        assert!((fader_position(-27.0) - 0.5).abs() < 1e-12);
    }
}
