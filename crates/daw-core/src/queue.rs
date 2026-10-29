//! A bounded single-producer single-consumer queue for crossing the real-time boundary.
//!
//! Use one queue per direction: commands from the UI/engine thread to the RT thread,
//! and meters or notifications from the RT thread back. `push` and `pop` are wait-free
//! and never allocate; all storage is allocated by [`spsc`].
//!
//! The queue is a small custom ring rather than a wrapper around `rtrb` so that its
//! atomics can be swapped for `loom`'s and the whole protocol model-checked.

use std::fmt;
use std::mem::MaybeUninit;
use std::sync::atomic::Ordering::{Acquire, Relaxed, Release};

use crate::sync::{Arc, AtomicUsize, CachePadded, UnsafeCell};

struct Shared<T> {
    /// Next slot to read. Written only by the consumer.
    head: CachePadded<AtomicUsize>,
    /// Next slot to write. Written only by the producer.
    tail: CachePadded<AtomicUsize>,
    capacity: usize,
    /// Power-of-two length so wrapping indices map to slots with a mask.
    slots: Box<[UnsafeCell<MaybeUninit<T>>]>,
}

// SAFETY: each slot is accessed by exactly one side at a time, handed over through
// the Release/Acquire pairs on `head` and `tail`.
unsafe impl<T: Send> Send for Shared<T> {}
unsafe impl<T: Send> Sync for Shared<T> {}

impl<T> Shared<T> {
    fn slot(&self, index: usize) -> &UnsafeCell<MaybeUninit<T>> {
        &self.slots[index & (self.slots.len() - 1)]
    }
}

impl<T> Drop for Shared<T> {
    fn drop(&mut self) {
        let mut head = self.head.load(Relaxed);
        let tail = self.tail.load(Relaxed);
        while head != tail {
            // SAFETY: slots in head..tail hold initialized values nobody else can reach.
            self.slot(head)
                .with_mut(|p| unsafe { (*p).assume_init_drop() });
            head = head.wrapping_add(1);
        }
    }
}

/// Creates a queue that holds up to `capacity` items.
pub fn spsc<T: Send>(capacity: usize) -> (Producer<T>, Consumer<T>) {
    let len = capacity.max(1).next_power_of_two();
    let shared = Arc::new(Shared {
        head: CachePadded(AtomicUsize::new(0)),
        tail: CachePadded(AtomicUsize::new(0)),
        capacity,
        slots: (0..len)
            .map(|_| UnsafeCell::new(MaybeUninit::uninit()))
            .collect(),
    });
    (
        Producer {
            shared: shared.clone(),
            tail: 0,
            head_cache: 0,
        },
        Consumer {
            shared,
            head: 0,
            tail_cache: 0,
        },
    )
}

/// The sending half of an [`spsc`] queue.
pub struct Producer<T> {
    shared: Arc<Shared<T>>,
    tail: usize,
    head_cache: usize,
}

impl<T> Producer<T> {
    /// Adds `value` to the queue, or returns it when the queue is full.
    pub fn push(&mut self, value: T) -> Result<(), T> {
        if self.free_cached() == 0 {
            self.head_cache = self.shared.head.load(Acquire);
            if self.free_cached() == 0 {
                return Err(value);
            }
        }
        // SAFETY: the slot is outside head..tail, so the consumer won't touch it
        // until the Release store below publishes it.
        self.shared
            .slot(self.tail)
            .with_mut(|p| unsafe { (*p).write(value) });
        self.tail = self.tail.wrapping_add(1);
        self.shared.tail.store(self.tail, Release);
        Ok(())
    }

    /// How many items can be pushed right now without failing.
    pub fn free(&mut self) -> usize {
        self.head_cache = self.shared.head.load(Acquire);
        self.free_cached()
    }

    pub fn capacity(&self) -> usize {
        self.shared.capacity
    }

    fn free_cached(&self) -> usize {
        self.shared.capacity - self.tail.wrapping_sub(self.head_cache)
    }
}

/// The receiving half of an [`spsc`] queue.
pub struct Consumer<T> {
    shared: Arc<Shared<T>>,
    head: usize,
    tail_cache: usize,
}

impl<T> Consumer<T> {
    /// Takes the oldest item, if any.
    pub fn pop(&mut self) -> Option<T> {
        if self.head == self.tail_cache {
            self.tail_cache = self.shared.tail.load(Acquire);
            if self.head == self.tail_cache {
                return None;
            }
        }
        // SAFETY: the slot is inside head..tail, published by the producer's Release.
        let value = self
            .shared
            .slot(self.head)
            .with_mut(|p| unsafe { (*p).assume_init_read() });
        self.head = self.head.wrapping_add(1);
        self.shared.head.store(self.head, Release);
        Some(value)
    }

    /// How many items are ready to pop.
    pub fn len(&mut self) -> usize {
        self.tail_cache = self.shared.tail.load(Acquire);
        self.tail_cache.wrapping_sub(self.head)
    }

    pub fn is_empty(&mut self) -> bool {
        self.len() == 0
    }

    pub fn capacity(&self) -> usize {
        self.shared.capacity
    }

    /// Pops items until the queue is empty. Items pushed meanwhile may be included.
    pub fn drain(&mut self) -> Drain<'_, T> {
        Drain(self)
    }
}

/// Iterator returned by [`Consumer::drain`].
pub struct Drain<'a, T>(&'a mut Consumer<T>);

impl<T> Iterator for Drain<'_, T> {
    type Item = T;
    fn next(&mut self) -> Option<T> {
        self.0.pop()
    }
}

impl<T> fmt::Debug for Producer<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Producer")
            .field("capacity", &self.shared.capacity)
            .finish_non_exhaustive()
    }
}

impl<T> fmt::Debug for Consumer<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Consumer")
            .field("capacity", &self.shared.capacity)
            .finish_non_exhaustive()
    }
}

#[cfg(all(test, not(loom)))]
mod tests {
    use super::*;

    #[test]
    fn fifo_and_capacity() {
        let (mut tx, mut rx) = spsc(3);
        assert_eq!(tx.capacity(), 3);
        for i in 0..3 {
            tx.push(i).unwrap();
        }
        assert_eq!(tx.push(3), Err(3));
        assert_eq!(rx.len(), 3);
        assert_eq!(rx.pop(), Some(0));
        tx.push(3).unwrap();
        assert_eq!(rx.drain().collect::<Vec<_>>(), [1, 2, 3]);
        assert_eq!(rx.pop(), None);
        assert_eq!(tx.free(), 3);
    }

    #[test]
    fn zero_capacity_is_always_full() {
        let (mut tx, mut rx) = spsc(0);
        assert_eq!(tx.push(1), Err(1));
        assert_eq!(rx.pop(), None);
    }

    #[test]
    fn drops_leftover_items() {
        let marker = std::sync::Arc::new(());
        let (mut tx, mut rx) = spsc(4);
        for _ in 0..3 {
            tx.push(marker.clone()).unwrap();
        }
        drop(rx.pop());
        drop((tx, rx));
        assert_eq!(std::sync::Arc::strong_count(&marker), 1);
    }

    #[test]
    fn threads_preserve_order() {
        const N: u64 = 200_000;
        let (mut tx, mut rx) = spsc(64);
        let producer = std::thread::spawn(move || {
            for i in 0..N {
                let mut v = i;
                while let Err(back) = tx.push(v) {
                    v = back;
                    std::hint::spin_loop();
                }
            }
        });
        let mut expected = 0;
        while expected < N {
            if let Some(v) = rx.pop() {
                assert_eq!(v, expected);
                expected += 1;
            }
        }
        producer.join().unwrap();
    }
}
