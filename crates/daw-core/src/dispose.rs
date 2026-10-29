//! Deferred drop: hand values off the real-time thread so their memory is freed elsewhere.
//!
//! Dropping a `Box`, `Vec` or the last `Arc` frees memory, which the RT thread must not
//! do. The RT side gives such values to a [`Retirer`] instead, and a normal thread calls
//! [`Collector::collect`] periodically to drop them.
//!
//! ```
//! use daw_core::dispose::deferred_drop;
//!
//! let (mut retirer, mut collector) = deferred_drop::<Box<[f32]>>(16);
//! // RT thread, after swapping in a new buffer:
//! let old: Box<[f32]> = vec![0.0; 1024].into_boxed_slice();
//! retirer.retire(old).ok();
//! // Normal thread, on a timer:
//! assert_eq!(collector.collect(), 1);
//! ```

use std::sync::atomic::Ordering::Relaxed;

use crate::queue::{Consumer, Producer, spsc};
use crate::sync::{Arc, AtomicU64};

/// Values of different types can share one collector as `Box<dyn Send>`.
pub type Garbage = Box<dyn Send>;

/// Creates a retirer/collector pair that holds up to `capacity` uncollected values.
pub fn deferred_drop<T: Send>(capacity: usize) -> (Retirer<T>, Collector<T>) {
    let (tx, rx) = spsc(capacity);
    let overflows = Arc::new(AtomicU64::new(0));
    (
        Retirer {
            tx,
            overflows: overflows.clone(),
        },
        Collector { rx, overflows },
    )
}

/// The RT side of a deferred-drop pair.
#[derive(Debug)]
pub struct Retirer<T> {
    tx: Producer<T>,
    overflows: Arc<AtomicU64>,
}

impl<T> Retirer<T> {
    /// Queues `value` to be dropped by the collector. Never allocates or frees.
    ///
    /// When the queue is full the value comes back and the overflow counter goes up.
    /// The caller then holds on to it and retries next cycle; dropping it on the RT
    /// thread is the last resort.
    pub fn retire(&mut self, value: T) -> Result<(), T> {
        self.tx.push(value).inspect_err(|_| {
            self.overflows.fetch_add(1, Relaxed);
        })
    }
}

/// The normal-thread side of a deferred-drop pair.
#[derive(Debug)]
pub struct Collector<T> {
    rx: Consumer<T>,
    overflows: Arc<AtomicU64>,
}

impl<T> Collector<T> {
    /// Drops everything retired so far and returns how many values were dropped.
    pub fn collect(&mut self) -> usize {
        self.rx.drain().count()
    }

    /// How many times `retire` found the queue full since the last call.
    pub fn take_overflows(&mut self) -> u64 {
        self.overflows.swap(0, Relaxed)
    }
}

#[cfg(all(test, not(loom)))]
mod tests {
    use super::*;

    #[test]
    fn collects_mixed_garbage() {
        let (mut retirer, mut collector) = deferred_drop::<Garbage>(2);
        let shared = std::sync::Arc::new(5);
        assert!(retirer.retire(Box::new(shared.clone())).is_ok());
        assert!(retirer.retire(Box::new(vec![1u8; 32])).is_ok());
        assert!(retirer.retire(Box::new(())).is_err());
        assert_eq!(std::sync::Arc::strong_count(&shared), 2);
        assert_eq!(collector.collect(), 2);
        assert_eq!(std::sync::Arc::strong_count(&shared), 1);
        assert_eq!(collector.take_overflows(), 1);
        assert_eq!(collector.take_overflows(), 0);
    }
}
