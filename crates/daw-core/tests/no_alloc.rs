//! Checks that the RT-side operations never touch the allocator.
#![cfg(not(loom))]

use daw_core::dispose::deferred_drop;
use daw_core::event::{Event, EventKind, EventList};
use daw_core::meter::PeakMeter;
use daw_core::node::{ProcessContext, TransportState};
use daw_core::plugin::PluginProcessor;
use daw_core::queue::spsc;
use daw_core::split::{SharedAudio, SplitAudio};
use daw_core::time::{
    Meter, MeterPoint, SampleTime, SegmentTempoMap, TempoMap, TickTime, Timeline,
};
use daw_test_support::no_alloc::allocations_in;

daw_test_support::install_checker!();

/// Counts the events it is given.
struct Counter(usize);

impl SplitAudio for Counter {
    fn process(&mut self, ctx: &mut ProcessContext) {
        self.0 += ctx.events_in.len();
    }
    fn reset(&mut self) {
        self.0 = 0;
    }
}

#[test]
fn rt_operations_do_not_allocate() {
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

    // A clone must keep the full capacity, not just its length.
    let mut source = EventList::with_capacity(8);
    source.push(ev(1)).unwrap();
    let mut cloned = source.clone();
    assert_eq!(
        allocations_in(|| {
            for f in 2..9 {
                cloned.push(ev(f)).unwrap();
            }
            assert!(cloned.push(ev(9)).is_err());
        }),
        0,
        "EventList::clone"
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

    let meter = Meter::new([
        MeterPoint {
            tick: TickTime(0),
            numerator: 4,
            denominator: 4,
        },
        MeterPoint {
            tick: TickTime(4 * 960),
            numerator: 7,
            denominator: 8,
        },
    ]);
    let timeline = Timeline::new(tempo, meter);
    assert_eq!(
        allocations_in(|| {
            let t = TransportState::at(&timeline, true, SampleTime(200_000), 48_000.0);
            t.advanced(4096);
        }),
        0,
        "timeline"
    );

    // A split plugin's audio half: blocks played, skipped and caught up, and a reset.
    let mut shared = SharedAudio::new(Counter(0));
    let mut half = shared.lend().unwrap();
    let mut events_out = EventList::with_capacity(0);
    let transport = TransportState::default();
    assert_eq!(
        allocations_in(|| {
            let mut block = |half: &mut dyn PluginProcessor| {
                half.process(&mut ProcessContext {
                    frames: 16,
                    transport: &transport,
                    audio_in: &[],
                    audio_out: &mut [],
                    events_in: &list,
                    events_out: &mut events_out,
                });
            };
            block(half.as_mut());
            shared.main(|_| block(half.as_mut()));
            block(half.as_mut());
            half.reset();
            block(half.as_mut());
        }),
        0,
        "split processor"
    );
    assert_eq!(
        shared.main(|c| c.0),
        1,
        "reset, then the last block's event"
    );
}
