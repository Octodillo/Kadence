//! Record arming and audio input choice on tracks.

use daw_model::bundle::{from_json, to_json};
use daw_model::commands::*;
use daw_model::*;

#[test]
fn arm_and_input_are_undoable_and_saved() {
    let mut doc = Document::new(Project::new("R"));
    let track = Track::new(TrackKind::Audio, "Mic");
    let id = track.id;
    doc.execute(AddTrack::new(0, track)).unwrap();
    doc.seal();

    doc.execute(SetTrack::new(id, TrackField::Armed(true)))
        .unwrap();
    doc.seal();
    let input = AudioInput {
        channels: InputChannels::Stereo(0),
        monitor: Monitoring::Auto,
    };
    doc.execute(SetTrack::new(id, TrackField::AudioInput(input)))
        .unwrap();
    doc.seal();
    let t = doc.project().track(id).unwrap();
    assert!(t.armed);
    assert_eq!(t.audio_input, input);
    assert_eq!(
        doc.history().undo_label().as_deref(),
        Some("Change Track Input")
    );

    let back = from_json(&to_json(doc.project()).unwrap()).unwrap();
    assert_eq!(back.track(id).unwrap().audio_input, input);
    assert!(back.track(id).unwrap().armed);

    doc.undo().unwrap();
    assert_eq!(
        doc.project().track(id).unwrap().audio_input,
        AudioInput::default()
    );
    assert_eq!(doc.history().undo_label().as_deref(), Some("Arm Track"));
    doc.undo().unwrap();
    assert!(!doc.project().track(id).unwrap().armed);
}

#[test]
fn files_without_the_fields_load_with_defaults() {
    let mut p = Project::new("Old");
    p.tracks.push(Track::new(TrackKind::Audio, "A").into());
    let mut json: serde_json::Value = serde_json::from_str(&to_json(&p).unwrap()).unwrap();
    let track = json["project"]["tracks"][0].as_object_mut().unwrap();
    assert!(track.remove("armed").is_some());
    assert!(track.remove("audio_input").is_some());
    let back = from_json(&json.to_string()).unwrap();
    let t = &back.tracks[0];
    assert!(!t.armed);
    assert_eq!(t.audio_input.channels, InputChannels::Mono(0));
    assert_eq!(t.audio_input.monitor, Monitoring::Off);
}

#[test]
fn only_audio_and_instrument_tracks_arm() {
    let mut doc = Document::new(Project::new("R"));
    let bus = Track::new(TrackKind::Bus, "Bus");
    let id = bus.id;
    doc.execute(AddTrack::new(0, bus)).unwrap();
    assert!(
        doc.execute(SetTrack::new(id, TrackField::Armed(true)))
            .is_err()
    );
    let master = doc.project().master.id;
    assert!(
        doc.execute(SetTrack::new(master, TrackField::Armed(true)))
            .is_err()
    );
    // Disarming is always allowed.
    doc.execute(SetTrack::new(id, TrackField::Armed(false)))
        .unwrap();
}
