mod support;

use daw_core::event::{Event, EventKind};
use daw_core::time::SampleTime;
use daw_midi::player::{Captured, InputGate};
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

fn live(kind: EventKind) -> Event {
    Event { frame: 0, kind }
}

fn on(channel: u8, key: u8) -> EventKind {
    EventKind::NoteOn {
        channel,
        key,
        velocity: 0.7,
        note_id: -1,
    }
}

fn off(channel: u8, key: Option<u8>) -> EventKind {
    EventKind::NoteOff {
        channel,
        key,
        velocity: 0.0,
        note_id: -1,
    }
}

#[test]
fn a_closed_gate_drops_live_input_but_ends_notes_it_let_through() {
    let mut rig = Rig::new();
    assert_eq!(rig.handle.input(), InputGate::default());
    let out = rig.block(0, 64, false, &[live(on(0, 60))]);
    assert_eq!(simple(&out), [(0, 60, true)]);

    rig.handle.set_input(InputGate {
        open: false,
        channel: None,
    });
    // The held note is ended when the gate closes, and new input is dropped.
    let out = rig.block(64, 64, false, &[live(on(0, 62))]);
    assert_eq!(simple(&out), [(64, 60, false)]);
    // Its real note-off and other messages don't get through.
    let out = rig.block(
        128,
        64,
        false,
        &[live(off(0, Some(60))), live(EventKind::Midi([0xB0, 1, 2]))],
    );
    assert!(out.is_empty(), "{out:?}");
    // Nothing is captured while closed, even armed and playing.
    rig.handle.set_armed(true);
    rig.block(192, 64, true, &[live(on(0, 64))]);
    assert!(
        rig.handle
            .drain_captured()
            .all(|c| !matches!(c, Captured::Event { .. }))
    );
}

#[test]
fn a_channel_gate_passes_one_channel() {
    let mut rig = Rig::new();
    rig.handle.set_input(InputGate {
        open: true,
        channel: Some(2),
    });
    assert_eq!(rig.handle.input().channel, Some(2));
    let out = rig.block(
        0,
        64,
        false,
        &[
            live(on(2, 60)),
            live(on(3, 61)),
            live(EventKind::Midi([0xB2, 7, 1])),
            live(EventKind::Midi([0xB5, 7, 1])),
            // System messages have no channel and pass.
            live(EventKind::Midi([0xF8, 0, 0])),
        ],
    );
    let kinds: Vec<_> = out.iter().map(|(_, k)| *k).collect();
    assert_eq!(
        kinds,
        [
            on(2, 60),
            EventKind::Midi([0xB2, 7, 1]),
            EventKind::Midi([0xF8, 0, 0])
        ]
    );
    // Switching channel ends the held note on the old one.
    rig.handle.set_input(InputGate {
        open: true,
        channel: Some(3),
    });
    let out = rig.block(64, 64, false, &[]);
    assert_eq!(simple(&out), [(64, 60, false)]);
}

#[test]
fn moving_the_gate_ends_every_live_note_on_the_old_channel() {
    let mut rig = Rig::new();
    rig.block(0, 64, false, &[live(on(0, 60)), live(on(0, 64))]);
    // Moving the gate ends both notes; a keyless note-off on the old
    // channel then has nothing left to end and is dropped like the rest.
    rig.handle.set_input(InputGate {
        open: true,
        channel: Some(5),
    });
    let out = rig.block(64, 64, false, &[]);
    assert_eq!(simple(&out), [(64, 60, false), (64, 64, false)]);
    let out = rig.block(128, 64, false, &[live(off(0, None))]);
    assert!(
        out.is_empty(),
        "nothing live on channel 0 any more: {out:?}"
    );
}

#[test]
fn a_take_ends_notes_the_gate_cut_off() {
    let mut rig = Rig::new();
    rig.handle.set_armed(true);
    rig.block(0, 64, true, &[live(on(0, 60))]);
    // Disarming the track (closing its gate) mid-note while recording.
    rig.handle.set_input(InputGate {
        open: false,
        channel: None,
    });
    rig.block(64, 64, true, &[]);
    // The real note-off arrives later and is not recorded twice.
    rig.block(128, 64, true, &[live(off(0, Some(60)))]);
    let events: Vec<_> = rig
        .handle
        .drain_captured()
        .filter_map(|c| match c {
            Captured::Event { time, kind } => Some((time, kind)),
            _ => None,
        })
        .collect();
    assert_eq!(
        events,
        [
            (SampleTime(0), on(0, 60)),
            (SampleTime(64), off(0, Some(60)))
        ]
    );
}

#[test]
fn a_take_keeps_the_end_of_a_note_that_started_through_the_gate() {
    let mut rig = Rig::new();
    rig.handle.set_armed(true);
    rig.handle.set_input(InputGate {
        open: true,
        channel: Some(0),
    });
    // A wildcard note-off on another channel would not pass; one that ends
    // a captured note does, even after the channel filter moved.
    rig.block(0, 64, true, &[live(on(0, 60))]);
    let out = rig.block(64, 64, true, &[live(off(0, Some(60)))]);
    assert_eq!(simple(&out), [(64, 60, false)]);
    let ends = rig
        .handle
        .drain_captured()
        .filter(|c| {
            matches!(
                c,
                Captured::Event {
                    kind: EventKind::NoteOff { .. },
                    ..
                }
            )
        })
        .count();
    assert_eq!(ends, 1);
}

#[test]
fn closing_the_gate_lifts_a_held_sustain_pedal() {
    let mut rig = Rig::new();
    let pedal_down = EventKind::Midi([0xB0, 64, 127]);
    rig.block(0, 64, false, &[live(pedal_down)]);
    rig.handle.set_input(InputGate {
        open: false,
        channel: None,
    });
    let out = rig.block(64, 64, false, &[]);
    let kinds: Vec<_> = out.iter().map(|(_, k)| *k).collect();
    assert_eq!(kinds, [EventKind::Midi([0xB0, 64, 0])]);
    // Only once.
    assert!(rig.block(128, 64, false, &[]).is_empty());
}

fn preview(key: u8) -> Option<daw_midi::player::Preview> {
    Some(daw_midi::player::Preview {
        key,
        channel: 0,
        velocity: 0.8,
    })
}

#[test]
fn preview_sounds_while_stopped_and_ends_on_request() {
    let mut rig = Rig::new();
    rig.handle.preview(preview(60));
    let out = rig.block(0, 64, false, &[]);
    assert_eq!(simple(&out), [(0, 60, true)]);
    // Nothing new: nothing sent.
    assert!(rig.block(0, 64, false, &[]).is_empty());
    // A new key ends the old one first.
    rig.handle.preview(preview(62));
    let out = rig.block(0, 64, false, &[]);
    assert_eq!(simple(&out), [(0, 60, false), (0, 62, true)]);
    rig.handle.preview(None);
    let out = rig.block(0, 64, false, &[]);
    assert_eq!(simple(&out), [(0, 62, false)]);
    assert!(rig.block(0, 64, false, &[]).is_empty());
}

#[test]
fn preview_of_the_same_key_retriggers() {
    let mut rig = Rig::new();
    rig.handle.preview(preview(60));
    rig.block(0, 64, false, &[]);
    rig.handle.preview(preview(60));
    let out = rig.block(0, 64, false, &[]);
    assert_eq!(simple(&out), [(0, 60, false), (0, 60, true)]);
}

#[test]
fn preview_bypasses_a_closed_gate_and_is_not_recorded() {
    let mut rig = Rig::new();
    rig.handle.set_input(InputGate {
        open: false,
        channel: None,
    });
    rig.handle.set_armed(true);
    rig.handle.preview(Some(daw_midi::player::Preview {
        key: 64,
        channel: 3,
        velocity: 0.5,
    }));
    let out = rig.block(0, 64, true, &[]);
    match out.as_slice() {
        [
            (
                0,
                EventKind::NoteOn {
                    channel: 3,
                    key: 64,
                    velocity,
                    ..
                },
            ),
        ] => assert!((velocity - 0.5).abs() < 0.01),
        other => panic!("unexpected {other:?}"),
    }
    rig.handle.preview(None);
    rig.block(64, 64, true, &[]);
    rig.handle.set_armed(false);
    rig.block(128, 64, true, &[]);
    let captured: Vec<Captured> = rig.handle.drain_captured().collect();
    assert!(
        captured
            .iter()
            .all(|c| !matches!(c, Captured::Event { .. })),
        "{captured:?}"
    );
}

#[test]
fn a_silent_preview_velocity_still_sounds() {
    let mut rig = Rig::new();
    rig.handle.preview(Some(daw_midi::player::Preview {
        key: 60,
        channel: 0,
        velocity: 0.0,
    }));
    let out = rig.block(0, 64, false, &[]);
    match out.as_slice() {
        [(0, EventKind::NoteOn { velocity, .. })] => assert!(*velocity > 0.0),
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn overlapping_notes_on_one_key_are_not_reattacked() {
    // The second note retriggers the key at 50: from then on only it
    // sounds, though the first one still spans the play position.
    let notes = vec![note(0, 1000, 60, 1), note(50, 500, 60, 2)];
    let mut rig = Rig::with(notes.clone());
    rig.play(0, 128, &[64]);
    // An edit elsewhere on the track sends the same notes again.
    rig.handle.set_sequence(Sequence::new(notes)).unwrap();
    assert_eq!(simple(&rig.play(128, 256, &[64])), []);
    // A loop wrap into the overlap restarts the key once, as the note
    // that was sounding there.
    let out = rig.play(100, 600, &[64]);
    assert_eq!(
        simple(&out),
        [(100, 60, false), (100, 60, true), (500, 60, false)]
    );
}

#[test]
fn a_note_cut_short_by_a_retrigger_is_not_chased() {
    // Played from the start, key 60 is silent after 60: the short note cut
    // the long one off at 50.
    let notes = vec![note(0, 200, 60, 1), note(50, 60, 60, 2)];
    let mut rig = Rig::with(notes.clone());
    assert_eq!(simple(&rig.play(0, 70, &[35])).len(), 4);
    rig.handle.set_sequence(Sequence::new(notes)).unwrap();
    assert_eq!(simple(&rig.play(70, 300, &[64])), []);
    // A locate there is silent too.
    assert_eq!(simple(&rig.play(70, 300, &[64])), []);
}

#[test]
fn a_note_added_behind_the_play_position_cuts_the_one_it_would_have() {
    let long = note(0, 1000, 60, 1);
    let mut rig = Rig::with(vec![long]);
    assert_eq!(simple(&rig.play(0, 128, &[64])), [(0, 60, true)]);
    // Played from the start, the short note ends the long one at 50.
    let edited = Sequence::new(vec![long, note(50, 60, 60, 2)]);
    rig.handle.set_sequence(edited).unwrap();
    assert_eq!(simple(&rig.play(128, 1100, &[64])), [(128, 60, false)]);
}

#[test]
fn chased_notes_that_do_not_fit_start_in_the_next_block() {
    let mut rig = Rig::with(vec![
        note(0, 1000, 60, 1),
        note(0, 1000, 62, 2),
        note(0, 1000, 64, 3),
    ]);
    rig.limit_output(2);
    let out = rig.block(500, 64, true, &[]);
    assert_eq!(simple(&out), [(500, 60, true), (500, 62, true)]);
    let out = rig.block(564, 64, true, &[]);
    assert_eq!(simple(&out), [(564, 64, true)]);
    assert!(rig.block(628, 64, true, &[]).is_empty());
}

#[test]
fn sequence_note_offs_that_do_not_fit_go_out_in_the_next_block() {
    let mut rig = Rig::with(vec![note(0, 1000, 60, 1), note(0, 1000, 62, 2)]);
    rig.play(0, 64, &[64]);
    rig.limit_output(2);
    // Playback stops in a block whose live input fills the list.
    let out = rig.block(64, 64, false, &[live(on(0, 40)), live(on(0, 41))]);
    assert_eq!(simple(&out), [(64, 40, true), (64, 41, true)]);
    let out = rig.block(64, 64, false, &[]);
    assert_eq!(simple(&out), [(64, 60, false), (64, 62, false)]);
    assert!(rig.block(64, 64, false, &[]).is_empty());
    // The overflow is counted, like the engine's dropped parameter changes.
    assert_eq!(rig.handle.take_event_overflows(), 2);
    assert_eq!(rig.handle.take_event_overflows(), 0);
}

#[test]
fn a_live_note_off_that_does_not_fit_goes_out_in_the_next_block() {
    let mut rig = Rig::new();
    rig.block(0, 64, false, &[live(on(0, 60)), live(on(1, 70))]);
    rig.limit_output(2);
    let input = [
        live(on(0, 61)),
        live(on(0, 62)),
        live(off(0, Some(60))),
        // Every note on channel 1.
        live(off(1, None)),
    ];
    let out = rig.block(64, 64, false, &input);
    assert_eq!(simple(&out), [(64, 61, true), (64, 62, true)]);
    let out = rig.block(128, 64, false, &[]);
    assert_eq!(simple(&out), [(128, 60, false), (128, 70, false)]);
    assert!(rig.block(192, 64, false, &[]).is_empty());
}

#[test]
fn a_pedal_lift_that_does_not_fit_goes_out_in_the_next_block() {
    let mut rig = Rig::new();
    rig.block(0, 64, false, &[live(EventKind::Midi([0xB0, 64, 127]))]);
    rig.limit_output(1);
    let input = [live(on(0, 61)), live(EventKind::Midi([0xB0, 64, 0]))];
    let out = rig.block(64, 64, false, &input);
    assert_eq!(simple(&out), [(64, 61, true)]);
    let out = rig.block(128, 64, false, &[]);
    let kinds: Vec<_> = out.iter().map(|(_, k)| *k).collect();
    assert_eq!(kinds, [EventKind::Midi([0xB0, 64, 0])]);
}

#[test]
fn notes_that_do_not_fit_start_in_the_next_block() {
    let mut rig = Rig::with(vec![
        note(10, 500, 60, 1),
        note(10, 500, 62, 2),
        note(10, 500, 64, 3),
    ]);
    rig.limit_output(2);
    let out = rig.block(0, 64, true, &[]);
    assert_eq!(simple(&out), [(10, 60, true), (10, 62, true)]);
    let out = rig.block(64, 64, true, &[]);
    assert_eq!(simple(&out), [(64, 64, true)]);
}

#[test]
fn a_preview_that_does_not_fit_sounds_in_the_next_block() {
    let mut rig = Rig::new();
    rig.limit_output(1);
    rig.handle.preview(preview(60));
    let out = rig.block(0, 64, false, &[live(on(0, 40))]);
    assert_eq!(simple(&out), [(0, 40, true)]);
    let out = rig.block(64, 64, false, &[]);
    assert_eq!(simple(&out), [(64, 60, true)]);
    // Its end waits for room too.
    rig.handle.preview(None);
    let out = rig.block(128, 64, false, &[live(on(0, 41))]);
    assert_eq!(simple(&out), [(128, 41, true)]);
    let out = rig.block(192, 64, false, &[]);
    assert_eq!(simple(&out), [(192, 60, false)]);
}

#[test]
fn a_full_list_never_leaves_a_sequence_note_sounding() {
    // More notes than fit, a live edit, a loop wrap and a stop, all with
    // room for three events a block.
    let notes = |shift: i64| -> Vec<_> {
        (0..40)
            .map(|i| note(i * 7 + shift, i * 7 + shift + 300, 40 + i as u8, i as u64))
            .collect()
    };
    let mut rig = Rig::with(notes(0));
    rig.limit_output(3);
    let mut out = rig.play(0, 400, &[64]);
    rig.handle.set_sequence(Sequence::new(notes(20))).unwrap();
    out.extend(rig.play(400, 600, &[64]));
    out.extend(rig.play(100, 300, &[64]));
    for _ in 0..40 {
        out.extend(rig.block(300, 64, false, &[]));
    }
    assert!(rig.handle.take_event_overflows() > 0);
    let mut sounding = std::collections::HashSet::new();
    for (_, kind) in &out {
        match *kind {
            EventKind::NoteOn { key, .. } => assert!(sounding.insert(key), "two voices on {key}"),
            EventKind::NoteOff { key, .. } => {
                sounding.remove(&key.unwrap());
            }
            _ => {}
        }
    }
    assert!(sounding.is_empty(), "still sounding: {sounding:?}");
    assert!(rig.block(300, 64, false, &[]).is_empty());
}
