//! The player's output does not depend on how the engine splits blocks,
//! and every note it starts is stopped.

mod support;

use std::collections::HashMap;

use daw_core::event::EventKind;
use daw_midi::sequence::SeqNote;
use proptest::prelude::*;
use support::{Rig, note};

fn notes() -> impl Strategy<Value = Vec<SeqNote>> {
    prop::collection::vec((0i64..4000, 1i64..1500, 56u8..68), 0..60).prop_map(|v| {
        v.into_iter()
            .enumerate()
            .map(|(i, (s, len, key))| note(s, s + len, key, i as u64))
            .collect()
    })
}

fn sizes() -> impl Strategy<Value = Vec<u32>> {
    prop::collection::vec(1u32..300, 1..8)
}

proptest! {
    #[test]
    fn block_size_does_not_matter(notes in notes(), a in sizes(), b in sizes()) {
        let mut x = Rig::with(notes.clone());
        let mut y = Rig::with(notes);
        let ex = x.play(0, 6000, &a);
        let ey = y.play(0, 6000, &b);
        prop_assert_eq!(ex, ey);
    }

    #[test]
    fn notes_are_balanced(notes in notes(), sizes in sizes(), jumps in prop::collection::vec(0i64..6000, 0..4)) {
        let mut rig = Rig::with(notes);
        let mut out = Vec::new();
        let mut from = 0;
        for j in jumps {
            out.extend(rig.play(from, from + 700, &sizes));
            from = j;
        }
        out.extend(rig.play(from, from + 700, &sizes));
        out.extend(rig.block(from + 700, 64, false, &[]));
        let mut sounding: HashMap<(u8, u8), i32> = HashMap::new();
        let mut last = i64::MIN;
        let mut segment_start = 0;
        for (t, k) in &out {
            if *t < last {
                // A jump: a new segment begins.
                segment_start = *t;
            }
            prop_assert!(*t >= segment_start);
            last = *t;
            match *k {
                EventKind::NoteOn { channel, key, note_id, .. } => {
                    prop_assert!(sounding.insert((channel, key), note_id).is_none(),
                        "two voices on one key");
                }
                EventKind::NoteOff { channel, key, note_id, .. } => {
                    prop_assert_eq!(sounding.remove(&(channel, key)), Some(note_id));
                }
                _ => {}
            }
        }
        prop_assert!(sounding.is_empty());
    }
}
