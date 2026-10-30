//! Live MIDI reaches the instrument tracks it should: per-track device and
//! channel filters, arming, and the input focus.

use std::collections::HashMap;

use daw_core::event::{Event, EventKind, EventList};
use daw_engine::{EngineConfig, EngineProcessor};
use daw_model::commands::{AddDevice, AddTrack, SetTrack, TrackField};
use daw_model::{Device, DeviceSource, Document, MidiInput, Project, Track, TrackId, TrackKind};
use daw_session::{Session, SessionConfig};

const BLOCK: usize = 256;

struct Rig {
    doc: Document,
    session: Session,
    proc: EngineProcessor,
}

impl Rig {
    fn new(tracks: usize) -> (Self, Vec<TrackId>) {
        let mut doc = Document::new(Project::new("live"));
        let mut ids = Vec::new();
        for i in 0..tracks {
            let track = Track::new(TrackKind::Instrument, format!("Synth {i}"));
            let id = track.id;
            doc.execute(AddTrack::new(i, track)).unwrap();
            let synth = Device::new(
                "Synth",
                DeviceSource::Builtin {
                    kind: "daw.synth".into(),
                },
            );
            doc.execute(AddDevice {
                track: id,
                index: 0,
                device: synth,
            })
            .unwrap();
            ids.push(id);
        }
        let (session, proc) = Session::new(SessionConfig {
            engine: EngineConfig {
                max_frames: BLOCK as u32,
                event_sources: 3,
                ..Default::default()
            },
            inputs: 0,
            outputs: 2,
        });
        let mut rig = Self { doc, session, proc };
        rig.session.set_midi_sources(HashMap::from([
            ("keys:a".to_owned(), 1),
            ("keys:b".to_owned(), 2),
        ]));
        rig.sync();
        (rig, ids)
    }

    fn sync(&mut self) {
        let snap = self.doc.snapshot();
        self.session.sync(&snap).unwrap();
    }

    fn set(&mut self, track: TrackId, field: TrackField) {
        self.doc.execute(SetTrack::new(track, field)).unwrap();
        self.sync();
    }

    /// Plays a note into `source` on `channel` and returns the loudest
    /// sample of the next few blocks.
    fn play_note(&mut self, source: usize, channel: u8) -> f32 {
        let mut on = EventList::with_capacity(4);
        on.push(Event {
            frame: 0,
            kind: EventKind::NoteOn {
                channel,
                key: 60,
                velocity: 0.9,
                note_id: -1,
            },
        })
        .unwrap();
        let none = EventList::with_capacity(0);
        let mut sources = [&none, &none, &none];
        sources[source] = &on;
        let mut peak = self.run(&sources);
        for _ in 0..8 {
            peak = peak.max(self.run(&[&none]));
        }
        // End it, so the next note starts from silence.
        let mut off = EventList::with_capacity(4);
        off.push(Event {
            frame: 0,
            kind: EventKind::NoteOff {
                channel,
                key: Some(60),
                velocity: 0.0,
                note_id: -1,
            },
        })
        .unwrap();
        let mut sources = [&none, &none, &none];
        sources[source] = &off;
        self.run(&sources);
        for _ in 0..200 {
            self.run(&[&none]);
        }
        peak
    }

    fn run(&mut self, sources: &[&EventList]) -> f32 {
        let mut l = vec![0.0; BLOCK];
        let mut r = vec![0.0; BLOCK];
        let mut outs: [&mut [f32]; 2] = [&mut l, &mut r];
        self.proc.process_sources(BLOCK, &[], &mut outs, sources);
        l.iter().fold(0.0, |m, v| m.max(v.abs()))
    }
}

const HEARD: f32 = 1e-3;

#[test]
fn a_track_hears_every_source_by_default() {
    let (mut rig, _) = Rig::new(1);
    for source in 0..3 {
        assert!(rig.play_note(source, 0) > HEARD, "source {source}");
    }
}

#[test]
fn a_device_filter_takes_only_that_device() {
    let (mut rig, t) = Rig::new(1);
    rig.set(
        t[0],
        TrackField::MidiInput(MidiInput {
            device: Some("keys:b".into()),
            channel: None,
        }),
    );
    assert!(rig.play_note(2, 0) > HEARD, "its device");
    assert!(rig.play_note(1, 0) < HEARD, "another device");
    assert!(rig.play_note(0, 0) < HEARD, "the general input");

    // A device that is not connected: nothing.
    rig.set(
        t[0],
        TrackField::MidiInput(MidiInput {
            device: Some("keys:gone".into()),
            channel: None,
        }),
    );
    for source in 0..3 {
        assert!(rig.play_note(source, 0) < HEARD, "source {source}");
    }
    // Plugging it in maps it to a source.
    rig.session
        .set_midi_sources(HashMap::from([("keys:gone".to_owned(), 1)]));
    assert!(rig.session.wants_sync());
    rig.sync();
    assert!(rig.play_note(1, 0) > HEARD);
}

#[test]
fn a_channel_filter_takes_only_that_channel() {
    let (mut rig, t) = Rig::new(1);
    rig.set(
        t[0],
        TrackField::MidiInput(MidiInput {
            device: None,
            channel: Some(3),
        }),
    );
    assert!(rig.play_note(1, 3) > HEARD);
    assert!(rig.play_note(1, 0) < HEARD);
}

#[test]
fn armed_tracks_take_live_input_and_others_do_not() {
    let (mut rig, t) = Rig::new(2);
    rig.set(t[1], TrackField::Armed(true));
    // Track 0 is not armed: with the armed one muted, nothing sounds.
    rig.set(t[1], TrackField::Mute(true));
    assert!(rig.play_note(0, 0) < HEARD);
    rig.set(t[1], TrackField::Mute(false));
    assert!(rig.play_note(0, 0) > HEARD);
}

#[test]
fn with_nothing_armed_the_focus_track_takes_live_input() {
    let (mut rig, t) = Rig::new(2);
    rig.set(t[0], TrackField::Mute(true));
    rig.session.set_input_focus(Some(t[0]));
    assert!(
        rig.play_note(0, 0) < HEARD,
        "only the muted focus track hears it"
    );
    rig.session.set_input_focus(Some(t[1]));
    assert!(rig.play_note(0, 0) > HEARD);
    // Arming wins over the focus.
    rig.set(t[0], TrackField::Armed(true));
    assert!(rig.play_note(0, 0) < HEARD);
}
