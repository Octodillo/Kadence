//! Audio clip fades: the schema 3 field, its migration and its edits.

use daw_core::time::{SampleTime, TickTime};
use daw_model::bundle;
use daw_model::commands::{AddClip, AddTrack, ClipField, SetClip};
use daw_model::*;
use serde_json::Value;

fn project_with_clip(content: ClipContent) -> (Project, ClipId) {
    let mut p = Project::new("Song");
    let mut t = Track::new(
        match content {
            ClipContent::Audio(_) => TrackKind::Audio,
            ClipContent::Midi(_) => TrackKind::Instrument,
        },
        "T",
    );
    let clip = Clip::new("c", TickTime(0), TickTime(3840), content);
    let id = clip.id;
    t.clips.push(clip.into());
    p.tracks.push(t.into());
    (p, id)
}

fn audio(p: &Project, id: ClipId) -> &AudioClip {
    match &p.clip(id).unwrap().content {
        ClipContent::Audio(a) => a,
        ClipContent::Midi(_) => panic!("not audio"),
    }
}

#[test]
fn version_2_audio_clips_load_without_fades() {
    let (p, id) = project_with_clip(ClipContent::Audio(AudioClip::new(
        "audio/a.wav",
        SampleTime(10),
    )));
    // As version 2 wrote it: no fade fields at all.
    let mut v: Value = serde_json::from_str(&bundle::to_json(&p).unwrap()).unwrap();
    v["version"] = 2.into();
    let content = &mut v["project"]["tracks"][0]["clips"][0]["content"];
    let obj = content.as_object_mut().unwrap();
    assert!(obj.remove("fade_in").is_some());
    assert!(obj.remove("fade_out").is_some());

    let loaded = bundle::from_json(&v.to_string()).unwrap();
    let a = audio(&loaded, id);
    assert_eq!(a.fade_in, Fade::default());
    assert_eq!(a.fade_out, Fade::default());
    assert!(a.fade_in.is_none());
    assert_eq!(a.offset, SampleTime(10));
}

#[test]
fn fades_round_trip_and_are_written_in_the_current_version() {
    let (mut p, id) = project_with_clip(ClipContent::Audio(AudioClip {
        fade_in: Fade::new(TickTime(120), FadeShape::SCurve),
        fade_out: Fade::new(TickTime(960), FadeShape::EqualPower),
        ..AudioClip::new("audio/a.wav", SampleTime(0))
    }));
    p.name = "Song".into();
    let json = bundle::to_json(&p).unwrap();
    let v: Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["version"], 3);
    let content = &v["project"]["tracks"][0]["clips"][0]["content"];
    assert_eq!(content["fade_in"]["length"], 120);
    assert_eq!(content["fade_in"]["shape"], "s_curve");
    assert_eq!(content["fade_out"]["shape"], "equal_power");
    let back = bundle::from_json(&json).unwrap();
    assert_eq!(back, p);
    assert_eq!(audio(&back, id).fade_out.length, TickTime(960));
}

#[test]
fn negative_fades_are_rejected() {
    let (p, _) = project_with_clip(ClipContent::Audio(AudioClip {
        fade_in: Fade::new(TickTime(-1), FadeShape::Linear),
        ..AudioClip::new("audio/a.wav", SampleTime(0))
    }));
    assert!(bundle::from_json(&bundle::to_json(&p).unwrap()).is_err());
}

#[test]
fn fade_edits_undo_merge_and_apply_to_audio_only() {
    let mut doc = Document::new(Project::new("Song"));
    let t = Track::new(TrackKind::Audio, "A");
    let tid = t.id;
    doc.execute(AddTrack::new(0, t)).unwrap();
    let clip = Clip::new(
        "c",
        TickTime(0),
        TickTime(3840),
        ClipContent::Audio(AudioClip::new("a.wav", SampleTime(0))),
    );
    let id = clip.id;
    doc.execute(AddClip::new(tid, clip)).unwrap();
    doc.seal();

    // A drag: many edits, one undo step.
    for len in [100, 200, 300] {
        doc.execute(SetClip::new(
            id,
            ClipField::FadeIn(Fade::new(TickTime(len), FadeShape::Linear)),
        ))
        .unwrap();
    }
    doc.seal();
    assert_eq!(audio(doc.project(), id).fade_in.length, TickTime(300));
    assert_eq!(
        doc.history().undo_label().as_deref(),
        Some("Change Fade In")
    );
    doc.undo().unwrap();
    assert!(audio(doc.project(), id).fade_in.is_none());
    doc.redo().unwrap();
    assert_eq!(audio(doc.project(), id).fade_in.length, TickTime(300));

    doc.execute(SetClip::new(
        id,
        ClipField::FadeOut(Fade::new(TickTime(50), FadeShape::SCurve)),
    ))
    .unwrap();
    assert_eq!(
        audio(doc.project(), id).fade_out,
        Fade::new(TickTime(50), FadeShape::SCurve)
    );
    assert!(
        doc.execute(SetClip::new(
            id,
            ClipField::FadeOut(Fade::new(TickTime(-5), FadeShape::Linear)),
        ))
        .is_err()
    );

    // MIDI clips have no fades.
    let inst = Track::new(TrackKind::Instrument, "I");
    let iid = inst.id;
    doc.execute(AddTrack::new(1, inst)).unwrap();
    let midi = Clip::new(
        "m",
        TickTime(0),
        TickTime(960),
        ClipContent::Midi(MidiClip::default()),
    );
    let mid = midi.id;
    doc.execute(AddClip::new(iid, midi)).unwrap();
    assert!(
        doc.execute(SetClip::new(mid, ClipField::FadeIn(Fade::default())))
            .is_err()
    );
}

#[test]
fn fade_shapes_run_from_silence_to_full() {
    for shape in FadeShape::ALL {
        assert_eq!(shape.gain(0.0), 0.0, "{shape:?}");
        assert!((shape.gain(1.0) - 1.0).abs() < 1e-6, "{shape:?}");
        let mut last = 0.0;
        for i in 0..=100 {
            let g = shape.gain(i as f32 / 100.0);
            assert!(g >= last, "{shape:?} rises");
            last = g;
        }
    }
    // Equal power: a fade-in and the mirrored fade-out sum to unit power.
    for i in 0..=10 {
        let x = i as f32 / 10.0;
        let (a, b) = (
            FadeShape::EqualPower.gain(x),
            FadeShape::EqualPower.gain(1.0 - x),
        );
        assert!((a * a + b * b - 1.0).abs() < 1e-5);
    }
    assert_eq!(fit_fades(100, 50, 300), (100, 50));
    assert_eq!(fit_fades(300, 100, 200), (150, 50));
    assert_eq!(fit_fades(-5, 10, 5), (0, 5));
}
