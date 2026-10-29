//! A MIDI part played through the real engine into the built-in synth.

use daw_core::node::Node;
use daw_core::time::{PPQ, SegmentTempoMap, TickTime};
use daw_engine::{GraphDesc, RenderOptions, render};
use daw_midi::note::NoteData;
use daw_midi::player::midi_player;
use daw_midi::sequence::{ClipNotes, compile};

fn rms(x: &[f32]) -> f32 {
    (x.iter().map(|s| s * s).sum::<f32>() / x.len().max(1) as f32).sqrt()
}

#[test]
fn clip_plays_through_the_synth() {
    // 120 bpm at 48 kHz: one beat is 24,000 samples. One note on beat 2
    // (of a clip that starts on beat 1), half a beat long.
    let tempo = SegmentTempoMap::new(120.0);
    let seq = compile(
        [ClipNotes {
            start: TickTime(PPQ),
            length: TickTime(4 * PPQ),
            notes: vec![(1, NoteData::new(PPQ, PPQ / 2, 69, 1.0))],
        }],
        &tempo,
        48_000.0,
    );
    assert_eq!(seq.notes()[0].start.0, 48_000);

    let (player, mut handle) = midi_player(16);
    handle.set_sequence(seq).unwrap();
    let synth: Box<dyn Node> = daw_devices::create("daw.synth").expect("synth");
    let mut g = GraphDesc::new(0, 2);
    let p = g.add_device("player", Box::new(player));
    let s = g.add_device("synth", synth);
    let track = g.add_track("track", 2);
    g.connect_events(p, s);
    g.connect(s, track);
    let out = render(
        g,
        &RenderOptions {
            frames: 120_000,
            block_size: 128,
            ..RenderOptions::default()
        },
    )
    .unwrap();
    // Interleaved stereo.
    let before = &out[..2 * 47_000];
    let during = &out[2 * 49_000..2 * 59_000];
    let after = &out[2 * 110_000..];
    assert!(
        rms(before) < 1e-6,
        "silent before the note: {}",
        rms(before)
    );
    assert!(
        rms(during) > 1e-3,
        "sounding during the note: {}",
        rms(during)
    );
    assert!(
        rms(after) < rms(during) / 100.0,
        "released after: {}",
        rms(after)
    );
}
