mod support;

use daw_core::event::{Event, EventKind};
use daw_core::time::SampleTime;
use daw_midi::player::Captured;
use daw_midi::sequence::Sequence;
use support::{Rig, note, simple};

#[test]
fn plays_notes_at_their_frames() {
    let mut rig = Rig::with(vec![note(10, 100, 60, 1), note(100, 150, 62, 2)]);
    let out = rig.play(0, 200, &[64]);
    assert_eq!(
        simple(&out),
        [
            (10, 60, true),
            (100, 60, false),
            (100, 62, true),
            (150, 62, false)
        ]
    );
    // Note ids pair on and off.
    let ids: Vec<i32> = out
        .iter()
        .map(|(_, k)| match *k {
            EventKind::NoteOn { note_id, .. } | EventKind::NoteOff { note_id, .. } => note_id,
            _ => -2,
        })
        .collect();
    assert_eq!(ids, [0, 0, 1, 1]);
}

#[test]
fn stop_ends_sounding_notes() {
    let mut rig = Rig::with(vec![note(10, 1000, 60, 1)]);
    rig.play(0, 64, &[64]);
    let out = rig.block(64, 64, false, &[]);
    assert_eq!(simple(&out), [(64, 60, false)]);
    // Staying stopped is silent.
    assert!(rig.block(64, 64, false, &[]).is_empty());
}

#[test]
fn locate_chases_spanning_notes() {
    let mut rig = Rig::with(vec![
        note(0, 1000, 60, 1),
        note(500, 600, 62, 2),
        note(700, 800, 64, 3),
    ]);
    rig.play(0, 64, &[64]);
    // Jump to 550: key 60 restarts, key 62 is chased, 64 plays normally.
    let out = rig.play(550, 800, &[64]);
    assert_eq!(
        simple(&out),
        [
            (550, 60, false),
            (550, 60, true),
            (550, 62, true),
            (600, 62, false),
            (700, 64, true)
        ]
    );
    rig.handle.set_chase(false);
    let out = rig.play(560, 600, &[64]);
    assert_eq!(simple(&out), [(560, 60, false), (560, 64, false)]);
}

#[test]
fn starting_playback_mid_note_chases() {
    let mut rig = Rig::with(vec![note(0, 1000, 60, 1)]);
    let out = rig.play(300, 400, &[64]);
    assert_eq!(simple(&out), [(300, 60, true)]);
}

#[test]
fn loop_wrap_restarts_cleanly() {
    // Loop 0..256 with a note crossing the loop end.
    let mut rig = Rig::with(vec![note(0, 50, 60, 1), note(200, 400, 62, 2)]);
    let mut out = Vec::new();
    for _ in 0..2 {
        out.extend(rig.play(0, 256, &[100]));
    }
    assert_eq!(
        simple(&out),
        [
            (0, 60, true),
            (50, 60, false),
            (200, 62, true),
            // The wrap: 62 stops at the loop start.
            (0, 62, false),
            (0, 60, true),
            (50, 60, false),
            (200, 62, true),
        ]
    );
}

#[test]
fn live_edits_update_sounding_notes() {
    let mut rig = Rig::with(vec![note(0, 1000, 60, 1), note(0, 1000, 64, 2)]);
    rig.play(0, 100, &[100]);
    // Shorten 60, delete 64, add 67 under the playhead (chased in) and a
    // later 69.
    rig.handle
        .set_sequence(Sequence::new(vec![
            note(0, 150, 60, 1),
            note(50, 500, 67, 3),
            note(300, 400, 69, 4),
        ]))
        .unwrap();
    let out = rig.play(100, 600, &[100]);
    assert_eq!(
        simple(&out),
        [
            (100, 64, false),
            (100, 67, true),
            (150, 60, false),
            (300, 69, true),
            (400, 69, false),
            (500, 67, false),
        ]
    );
    // Shortening a note to before the playhead stops it at once.
    let mut rig = Rig::with(vec![note(0, 1000, 60, 1)]);
    rig.play(0, 100, &[100]);
    rig.handle
        .set_sequence(Sequence::new(vec![note(0, 50, 60, 1)]))
        .unwrap();
    assert_eq!(simple(&rig.play(100, 200, &[100])), [(100, 60, false)]);
    assert!(rig.handle.collect_garbage() >= 1);
}

#[test]
fn retrigger_ends_the_previous_note() {
    let mut rig = Rig::with(vec![note(0, 100, 60, 1), note(50, 80, 60, 2)]);
    let out = rig.play(0, 200, &[64]);
    assert_eq!(
        simple(&out),
        [
            (0, 60, true),
            (50, 60, false),
            (50, 60, true),
            (80, 60, false)
        ]
    );
}

#[test]
fn input_passes_through_and_is_captured_when_armed() {
    let mut rig = Rig::new();
    let input = [
        Event {
            frame: 3,
            kind: EventKind::NoteOn {
                channel: 0,
                key: 72,
                velocity: 1.0,
                note_id: -1,
            },
        },
        Event {
            frame: 4,
            kind: EventKind::ParamValue {
                param: 1,
                value: 0.5,
            },
        },
    ];
    // Stopped: thru only.
    assert_eq!(rig.block(0, 64, false, &input).len(), 2);
    assert_eq!(rig.handle.drain_captured().count(), 0);
    rig.handle.set_armed(true);
    // Stopped and armed: still nothing captured.
    rig.block(0, 64, false, &input);
    assert_eq!(rig.handle.drain_captured().count(), 0);
    rig.block(1000, 64, true, &input);
    rig.block(1064, 64, true, &[]);
    rig.block(0, 64, true, &[]);
    rig.block(64, 64, false, &[]);
    let captured: Vec<Captured> = rig.handle.drain_captured().collect();
    assert_eq!(
        captured,
        [
            Captured::Event {
                time: SampleTime(1003),
                kind: input[0].kind
            },
            Captured::Jump {
                from: SampleTime(1128),
                to: SampleTime(0)
            },
            Captured::Stop { at: SampleTime(64) },
        ]
    );
    // Disarming while playing ends the take.
    rig.block(0, 64, true, &[]);
    rig.handle.set_armed(false);
    rig.block(64, 64, true, &input);
    let captured: Vec<Captured> = rig.handle.drain_captured().collect();
    assert_eq!(captured, [Captured::Stop { at: SampleTime(64) }]);
}

#[test]
fn polyphony_is_capped() {
    let notes = (0..300u64)
        .map(|i| note(i as i64, 10_000 + i as i64, (i % 128) as u8, i))
        .collect();
    // Keys repeat every 128 notes, so retriggers keep it at 128 voices;
    // check the hard cap separately with distinct channels.
    let mut rig = Rig::with(notes);
    let out = rig.play(0, 400, &[64]);
    let ons = out
        .iter()
        .filter(|(_, k)| matches!(k, EventKind::NoteOn { .. }))
        .count();
    assert_eq!(ons, 300);
    let mut many: Vec<_> = (0..300u64)
        .map(|i| {
            let mut n = note(i as i64, 10_000, (i % 128) as u8, i);
            n.channel = (i / 128) as u8;
            n
        })
        .collect();
    many.sort_by_key(|n| n.start);
    let mut rig = Rig::with(many);
    let out = rig.play(0, 400, &[64]);
    let ons = out
        .iter()
        .filter(|(_, k)| matches!(k, EventKind::NoteOn { .. }))
        .count();
    let offs = out
        .iter()
        .filter(|(_, k)| matches!(k, EventKind::NoteOff { .. }))
        .count();
    assert_eq!(ons - offs, daw_midi::player::MAX_ACTIVE);
}

#[test]
fn recording_round_trip() {
    use daw_core::time::{PPQ, SegmentTempoMap, TickTime};
    use daw_midi::record::{Recorder, to_clip_notes};

    let ev = |frame, kind| Event { frame, kind };
    let on = |key| EventKind::NoteOn {
        channel: 0,
        key,
        velocity: 0.5,
        note_id: -1,
    };
    let off = |key| EventKind::NoteOff {
        channel: 0,
        key: Some(key),
        velocity: 0.0,
        note_id: -1,
    };
    let mut rig = Rig::new();
    rig.handle.set_armed(true);
    // Blocks around beat 2 at 120 bpm with a locate to just before 48,000
    // and a wrap back to 24,000, while key 62 is held.
    rig.block(24_000, 128, true, &[ev(0, on(60))]);
    rig.block(24_128, 128, true, &[ev(10, off(60)), ev(20, on(62))]);
    rig.block(47_872, 128, true, &[]);
    rig.block(24_000, 128, true, &[ev(64, off(62))]);
    rig.block(24_128, 128, false, &[]);
    let mut rec = Recorder::new();
    for c in rig.handle.drain_captured() {
        rec.feed(c);
    }
    let take = rec.finish(None);
    let spans: Vec<(i64, i64, u8)> = take.iter().map(|n| (n.start.0, n.end.0, n.key)).collect();
    assert_eq!(
        spans,
        [
            (24_000, 24_138, 60),
            (24_000, 24_064, 62),
            (24_148, 24_256, 62),
            (47_872, 48_000, 62)
        ]
    );
    let tempo = SegmentTempoMap::new(120.0);
    let notes = to_clip_notes(&take, &tempo, 48_000.0, TickTime(PPQ)..TickTime(2 * PPQ));
    assert_eq!(notes[3].end().0, PPQ);
    assert!(notes.iter().all(|n| n.is_valid()));
}

#[test]
fn transposing_a_sounding_note_moves_it() {
    let mut rig = Rig::with(vec![note(0, 1000, 60, 1)]);
    rig.play(0, 100, &[100]);
    rig.handle
        .set_sequence(Sequence::new(vec![note(0, 1000, 64, 1)]))
        .unwrap();
    let out = rig.play(100, 1100, &[100]);
    assert_eq!(
        simple(&out),
        [(100, 60, false), (100, 64, true), (1000, 64, false)]
    );
}

#[test]
fn reset_ends_a_capture() {
    use daw_core::node::Node;
    let mut rig = Rig::new();
    rig.handle.set_armed(true);
    rig.block(0, 64, true, &[]);
    rig.player.reset();
    rig.block(5000, 64, true, &[]);
    let captured: Vec<Captured> = rig.handle.drain_captured().collect();
    assert_eq!(captured, [Captured::Stop { at: SampleTime(64) }]);
}
