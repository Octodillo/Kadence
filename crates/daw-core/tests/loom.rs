//! Model-checks the cross-thread primitives. Run with:
//! `RUSTFLAGS="--cfg loom" cargo test -p daw-core --release --test loom`
#![cfg(loom)]

use loom::sync::Arc;
use loom::thread;

use daw_core::dispose::deferred_drop;
use daw_core::exclusive::Exclusive;
use daw_core::meter::PeakMeter;
use daw_core::queue::spsc;
use daw_core::rt_log;
use daw_core::rtlog::{RtLevel, rt_log};

#[test]
fn spsc_delivers_in_order_across_wraparound() {
    loom::model(|| {
        let (mut tx, mut rx) = spsc::<usize>(2);
        let producer = thread::spawn(move || {
            let mut sent = 0;
            for i in 0..3 {
                if tx.push(i).is_ok() {
                    sent += 1;
                }
            }
            sent
        });
        let mut got = Vec::new();
        for _ in 0..3 {
            if let Some(v) = rx.pop() {
                got.push(v);
            }
        }
        let sent = producer.join().unwrap();
        got.extend(rx.drain());
        // Pushes only fail while the queue is full, so what arrives is a prefix, in order.
        assert_eq!(got, (0..sent).collect::<Vec<_>>());
        assert!(sent >= 2);
    });
}

#[test]
fn spsc_drops_unread_items_once() {
    loom::model(|| {
        let marker = Arc::new(());
        let (mut tx, mut rx) = spsc::<Arc<()>>(2);
        let m = marker.clone();
        let producer = thread::spawn(move || {
            let _ = tx.push(m.clone());
            let _ = tx.push(m);
        });
        drop(rx.pop());
        producer.join().unwrap();
        drop(rx);
        assert_eq!(Arc::strong_count(&marker), 1);
    });
}

#[test]
fn retired_values_are_dropped_by_collector() {
    loom::model(|| {
        let marker = Arc::new(());
        let (mut retirer, mut collector) = deferred_drop::<Arc<()>>(1);
        let m = marker.clone();
        let rt = thread::spawn(move || {
            let mut v = m;
            // Retry like an RT thread would on the next cycle.
            loop {
                match retirer.retire(v) {
                    Ok(()) => break,
                    Err(back) => {
                        v = back;
                        thread::yield_now();
                    }
                }
            }
        });
        let mut dropped = 0;
        while dropped == 0 {
            dropped += collector.collect();
            thread::yield_now();
        }
        rt.join().unwrap();
        assert_eq!(Arc::strong_count(&marker), 1);
    });
}

#[test]
fn rt_log_counts_every_record() {
    loom::model(|| {
        let (mut log, mut drain) = rt_log(1);
        let rt = thread::spawn(move || {
            rt_log!(log, RtLevel::Warn, "a {}", 1);
            rt_log!(log, RtLevel::Warn, "b {}", 2);
        });
        let mut seen = drain.pop().map_or(0, |_| 1);
        rt.join().unwrap();
        seen += drain.drain().count() as u64;
        assert_eq!(seen + drain.take_dropped(), 2);
    });
}

#[test]
fn peak_meter_never_loses_the_max() {
    loom::model(|| {
        let meter = Arc::new(PeakMeter::new());
        let m = meter.clone();
        let rt = thread::spawn(move || {
            m.update_peak(0.5);
            m.update_peak(0.25);
        });
        let first = meter.take();
        rt.join().unwrap();
        let second = meter.take();
        assert_eq!(first.max(second), 0.5);
    });
}

#[test]
fn exclusive_never_lets_both_threads_in() {
    loom::model(|| {
        // Each side checks that nobody else is inside, then leaves a mark.
        let x = Arc::new(Exclusive::new((false, 0u32)));
        let audio = x.clone();
        let rt = thread::spawn(move || {
            let mut ran = 0;
            for _ in 0..2 {
                let got = audio.try_audio(|(inside, n)| {
                    assert!(!*inside);
                    *inside = true;
                    *n += 1;
                    *inside = false;
                });
                ran += got.is_some() as u32;
            }
            ran
        });
        x.main(|(inside, n)| {
            assert!(!*inside);
            *inside = true;
            *n += 10;
            *inside = false;
        });
        let ran = rt.join().unwrap();
        x.main(|(_, n)| assert_eq!(*n, 10 + ran));
    });
}
