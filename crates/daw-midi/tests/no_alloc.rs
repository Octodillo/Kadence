//! `MidiPlayer::process` must not allocate or free, including when it picks
//! up a new sequence, chases notes, retriggers and captures input.

mod support;

use daw_core::event::{Event, EventKind};
use daw_midi::sequence::Sequence;
use daw_test_support::no_alloc::allocations_in;
use support::{Rig, note};

daw_test_support::install_checker!();

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
    let mut total = 0;
    // A roomy output list, and one that overflows and owes note-offs.
    for capacity in [1024, 8] {
        total += violations_playing(capacity, &seq);
    }
    assert_eq!(total, 0);
}

fn violations_playing(capacity: usize, seq: &impl Fn(i64) -> Sequence) -> u32 {
    let mut rig = Rig::new();
    rig.limit_output(capacity);
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
        if round % 3 == 0 {
            let key = (round % 2 == 0).then_some(daw_midi::player::Preview {
                key: 50 + (round % 20) as u8,
                channel: 0,
                velocity: 0.5,
            });
            rig.handle.preview(key);
        }
        // Jump every so often to exercise chase.
        if round % 7 == 0 {
            pos = (round * 1234) % 15_000;
        }
        let playing = round % 23 != 0;
        total += allocations_in(|| rig.block_into(pos, 128, playing, &input));
        pos += 128;
    }
    total
}
