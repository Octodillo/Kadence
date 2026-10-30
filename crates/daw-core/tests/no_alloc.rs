//! Checks that the RT-side operations never touch the allocator.
//!
//! `warn_debug` makes violations count instead of abort, so everything runs inside one
//! test: the violation counter is global and parallel tests would see each other's.
//! `assert_no_alloc` checks nothing without debug assertions and leaves out the items
//! used here, so the binary is empty in release builds.
#![cfg(all(not(loom), debug_assertions))]

use assert_no_alloc::{AllocDisabler, assert_no_alloc, reset_violation_count, violation_count};
use daw_core::dispose::deferred_drop;
use daw_core::event::{Event, EventKind, EventList};
use daw_core::meter::PeakMeter;
use daw_core::queue::spsc;
use daw_core::rt_log;
use daw_core::rtlog::{RtLevel, rt_log};
use daw_core::time::{SegmentTempoMap, TempoMap, TickTime};

#[global_allocator]
static ALLOC: AllocDisabler = AllocDisabler;

fn allocations_in(f: impl FnOnce()) -> u32 {
    reset_violation_count();
    assert_no_alloc(f);
    violation_count()
}

#[test]
fn rt_operations_do_not_allocate() {
    // The checker itself must notice an allocation.
    assert!(allocations_in(|| drop(vec![1u8])) > 0);

    let mut list = EventList::with_capacity(64);
    let ev = |frame| Event {
        frame,
        kind: EventKind::NoteOn {
            channel: 0,
            key: 60,
            velocity: 1.0,
            note_id: -1,
        },
    };
    assert_eq!(
        allocations_in(|| {
            for f in (0..64).rev() {
                list.push(ev(f)).unwrap();
            }
            assert!(list.push(ev(0)).is_err());
            assert_eq!(list.iter().count(), 64);
            list.clear();
            list.push(ev(3)).unwrap();
        }),
        0,
        "EventList"
    );

    let (mut tx, mut rx) = spsc::<[f32; 8]>(16);
    assert_eq!(
        allocations_in(|| {
            for _ in 0..100 {
                let _ = tx.push([0.5; 8]);
                let _ = rx.pop();
            }
        }),
        0,
        "spsc"
    );

    let (mut retirer, mut collector) = deferred_drop::<Vec<f32>>(4);
    let mut buf = Some(vec![0.0f32; 1024]);
    assert_eq!(
        allocations_in(|| retirer.retire(buf.take().unwrap()).unwrap()),
        0,
        "retire"
    );
    assert_eq!(collector.collect(), 1);

    let (mut log, mut drain) = rt_log(8);
    assert_eq!(
        allocations_in(|| {
            for i in 0..16 {
                rt_log!(log, RtLevel::Warn, "cycle {} load {}", i, 0.5f32);
            }
        }),
        0,
        "rt_log"
    );
    assert_eq!(drain.drain().count(), 8);
    assert_eq!(drain.take_dropped(), 8);

    let meter = PeakMeter::new();
    assert_eq!(allocations_in(|| meter.update(&[0.25; 64])), 0, "meter");

    let mut tempo = SegmentTempoMap::new(120.0);
    tempo.set_tempo(TickTime(960), 90.0, true);
    tempo.set_tempo(TickTime(9600), 140.0, false);
    assert_eq!(
        allocations_in(|| {
            let s = tempo.ticks_to_samples(TickTime(5000), 48_000.0);
            tempo.samples_to_ticks(s, 48_000.0);
            tempo.bpm_at(TickTime(5000));
        }),
        0,
        "tempo map"
    );
}
