//! Logging from the real-time thread without formatting, allocating or locking.
//!
//! The RT thread records a static message plus up to [`MAX_ARGS`] plain values into a
//! queue with [`rt_log!`]. A normal thread drains the records and formats them, where
//! `{}` in the message is replaced by the next argument.
//!
//! ```
//! use daw_core::rt_log;
//! use daw_core::rtlog::{RtLevel, rt_log};
//!
//! let (mut log, mut drain) = rt_log(64);
//! rt_log!(log, RtLevel::Warn, "xrun after {} frames, load {}", 128u32, 0.97);
//! let rec = drain.pop().unwrap();
//! assert_eq!(rec.to_string(), "xrun after 128 frames, load 0.97");
//! ```

use std::fmt;
use std::sync::atomic::Ordering::Relaxed;

use crate::queue::{Consumer, Producer, spsc};
use crate::sync::{Arc, AtomicU64};

pub const MAX_ARGS: usize = 4;

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RtLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

/// A value captured by [`rt_log!`].
#[derive(Copy, Clone, Debug, PartialEq, Default)]
pub enum RtValue {
    #[default]
    None,
    I64(i64),
    U64(u64),
    F64(f64),
    Bool(bool),
    Str(&'static str),
}

macro_rules! rt_value_from {
    ($($t:ty => $variant:ident as $as:ty),* $(,)?) => {$(
        impl From<$t> for RtValue {
            fn from(v: $t) -> Self {
                RtValue::$variant(v as $as)
            }
        }
    )*};
}

rt_value_from! {
    i8 => I64 as i64, i16 => I64 as i64, i32 => I64 as i64, i64 => I64 as i64, isize => I64 as i64,
    u8 => U64 as u64, u16 => U64 as u64, u32 => U64 as u64, u64 => U64 as u64, usize => U64 as u64,
    f32 => F64 as f64, f64 => F64 as f64,
}

impl From<bool> for RtValue {
    fn from(v: bool) -> Self {
        RtValue::Bool(v)
    }
}

impl From<&'static str> for RtValue {
    fn from(v: &'static str) -> Self {
        RtValue::Str(v)
    }
}

impl fmt::Display for RtValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RtValue::None => Ok(()),
            RtValue::I64(v) => v.fmt(f),
            RtValue::U64(v) => v.fmt(f),
            RtValue::F64(v) => v.fmt(f),
            RtValue::Bool(v) => v.fmt(f),
            RtValue::Str(v) => v.fmt(f),
        }
    }
}

/// One log entry, formatted lazily through `Display`.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct RtRecord {
    pub level: RtLevel,
    pub message: &'static str,
    pub args: [RtValue; MAX_ARGS],
    pub arg_count: u8,
}

impl RtRecord {
    pub fn args(&self) -> &[RtValue] {
        &self.args[..self.arg_count as usize]
    }
}

impl fmt::Display for RtRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut args = self.args().iter();
        let mut pieces = self.message.split("{}");
        if let Some(first) = pieces.next() {
            f.write_str(first)?;
        }
        for piece in pieces {
            match args.next() {
                Some(arg) => arg.fmt(f)?,
                None => f.write_str("{}")?,
            }
            f.write_str(piece)?;
        }
        Ok(())
    }
}

/// Creates a logger for the RT thread and a drain for a normal thread.
pub fn rt_log(capacity: usize) -> (RtLogger, RtLogDrain) {
    let (tx, rx) = spsc(capacity);
    let dropped = Arc::new(AtomicU64::new(0));
    (
        RtLogger {
            tx,
            dropped: dropped.clone(),
        },
        RtLogDrain { rx, dropped },
    )
}

/// The RT side of the log. Never allocates, locks or formats.
#[derive(Debug)]
pub struct RtLogger {
    tx: Producer<RtRecord>,
    dropped: Arc<AtomicU64>,
}

impl RtLogger {
    /// Records `message` with the first [`MAX_ARGS`] of `args`. When the queue is full
    /// the record is dropped and counted. Prefer the [`rt_log!`] macro.
    pub fn log(&mut self, level: RtLevel, message: &'static str, args: &[RtValue]) {
        let mut rec = RtRecord {
            level,
            message,
            args: [RtValue::None; MAX_ARGS],
            arg_count: args.len().min(MAX_ARGS) as u8,
        };
        rec.args[..rec.arg_count as usize].copy_from_slice(&args[..rec.arg_count as usize]);
        if self.tx.push(rec).is_err() {
            self.dropped.fetch_add(1, Relaxed);
        }
    }
}

/// The normal-thread side of the log.
#[derive(Debug)]
pub struct RtLogDrain {
    rx: Consumer<RtRecord>,
    dropped: Arc<AtomicU64>,
}

impl RtLogDrain {
    pub fn pop(&mut self) -> Option<RtRecord> {
        self.rx.pop()
    }

    /// Pops every pending record.
    pub fn drain(&mut self) -> impl Iterator<Item = RtRecord> + '_ {
        self.rx.drain()
    }

    /// How many records were lost to a full queue since the last call.
    pub fn take_dropped(&mut self) -> u64 {
        self.dropped.swap(0, Relaxed)
    }
}

/// Logs from the RT thread: `rt_log!(logger, RtLevel::Warn, "x = {}", x)`.
///
/// Arguments must convert into [`RtValue`]; extra arguments past [`MAX_ARGS`] are ignored.
#[macro_export]
macro_rules! rt_log {
    ($logger:expr, $level:expr, $msg:literal $(, $arg:expr)* $(,)?) => {
        $logger.log($level, $msg, &[$($crate::rtlog::RtValue::from($arg)),*])
    };
}

#[cfg(all(test, not(loom)))]
mod tests {
    use super::*;

    #[test]
    fn formats_placeholders() {
        let (mut log, mut drain) = rt_log(4);
        rt_log!(log, RtLevel::Info, "plain");
        rt_log!(log, RtLevel::Info, "{} and {} and {}", -3i32, true, "s");
        rt_log!(log, RtLevel::Info, "{} {}", 1u8);
        rt_log!(log, RtLevel::Error, "{}{}{}{}{}", 1, 2, 3, 4, 5);
        let out: Vec<_> = drain.drain().map(|r| r.to_string()).collect();
        assert_eq!(out, ["plain", "-3 and true and s", "1 {}", "1234{}"]);
    }

    #[test]
    fn counts_dropped_records() {
        let (mut log, mut drain) = rt_log(1);
        rt_log!(log, RtLevel::Warn, "a");
        rt_log!(log, RtLevel::Warn, "b");
        rt_log!(log, RtLevel::Warn, "c");
        assert_eq!(drain.take_dropped(), 2);
        assert_eq!(drain.pop().map(|r| r.message), Some("a"));
        assert_eq!(drain.pop(), None);
    }
}
