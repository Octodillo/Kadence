//! `MidiPlayer::process` must not allocate or free, including when it picks
//! up a new sequence, chases notes, retriggers and captures input.

mod support;

use assert_no_alloc::{AllocDisabler, assert_no_alloc, reset_violation_count, violation_count};
use daw_core::event::{Event, EventKind};
use daw_midi::sequence::Sequence;
use support::{Rig, note};

#[global_allocator]
static ALLOC: AllocDisabler = AllocDisabler;

fn violations(f: impl FnOnce()) -> u32 {
    reset_violation_count();
    assert_no_alloc(f);
    violation_count()
}

#[test]
fn detector_catches_allocations() {
    assert!(violations(|| drop(std::hint::black_box(vec![1u8; 16]))) > 0);
}

#[test]
fn process_never_allocates() {
    let seq = |shift: i64| {
        Sequence::new(
            (0..400)
                .map(|i| {
                    note(
                        i * 37 + shift,
                        i * 37 + shift + 500,
                        40 + (i % 40) as u8,
                        i as u64,
                    )
                })
                .collect(),
        )
    };
    let mut rig = Rig::new();
    rig.handle.set_sequence(seq(0)).unwrap();
    rig.handle.set_armed(true);
    let input = [Event {
        frame: 1,
        kind: EventKind::NoteOn {
            channel: 0,
            key: 60,
            velocity: 1.0,
            note_id: -1,
        },
    }];
    let mut total = 0;
    let mut pos = 0;
    for round in 0..200 {
        if round % 10 == 0 {
            // Built here, outside the checked region, like the UI thread would.
            let _ = rig.handle.set_sequence(seq(round));
            rig.handle.collect_garbage();
            rig.handle.drain_captured().for_each(drop);
        }
        // Jump every so often to exercise chase.
        if round % 7 == 0 {
            pos = (round * 1234) % 15_000;
        }
        let playing = round % 23 != 0;
        total += violations(|| rig.block_into(pos, 128, playing, &input));
        pos += 128;
    }
    assert_eq!(total, 0);
}
