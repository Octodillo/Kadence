//! A small generated song for screenshots and the self test.

use std::f32::consts::TAU;
use std::sync::Arc;

use daw_core::time::{PPQ, SampleTime, TickTime};
use daw_media::{AudioData, WavFormat, write_wav};
use daw_model::{
    AudioClip, AutomationLane, AutomationPoint, AutomationTarget, Clip, ClipContent, Color, Device,
    DeviceSource, Document, Fade, FadeShape, MidiClip, Note, Project, Send, Track, TrackKind,
};

const RATE: u32 = 48_000;
const BAR: i64 = 4 * PPQ;

/// A plucked tone that decays over each beat at 120 bpm, `beats` long.
fn pluck(freq: f32, beats: usize) -> AudioData {
    let beat = RATE as usize / 2;
    let mut l = Vec::with_capacity(beat * beats);
    for i in 0..beat * beats {
        let t = (i % beat) as f32 / RATE as f32;
        let env = (-t * 6.0).exp();
        let x = (TAU * freq * i as f32 / RATE as f32).sin() * env * 0.7;
        l.push(x + 0.15 * (TAU * 2.0 * freq * i as f32 / RATE as f32).sin() * env);
    }
    let r = l.iter().map(|x| x * 0.8).collect();
    AudioData {
        sample_rate: RATE,
        channels: vec![l, r],
    }
}

/// Builds the demo in a temporary bundle so the audio has somewhere to live.
pub fn build() -> Result<Document, String> {
    // Numbered, so tests building it at once don't share a directory.
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir()
        .join(format!("kadence-demo-{}-{n}", std::process::id()))
        .join("Demo");
    let _ = std::fs::remove_dir_all(&dir);
    let audio = dir.join("audio");
    std::fs::create_dir_all(&audio).map_err(|e| e.to_string())?;
    let bass = audio.join("bass.wav");
    let lead = audio.join("lead.wav");
    write_wav(&bass, &pluck(55.0, 16), WavFormat::Float32).map_err(|e| e.to_string())?;
    write_wav(&lead, &pluck(440.0, 8), WavFormat::Float32).map_err(|e| e.to_string())?;

    let mut project = Project::new("Demo");
    let audio_clip = |name: &str, file: &str, start: i64, bars: i64, offset: i64| {
        Arc::new(Clip::new(
            name,
            TickTime(start * BAR),
            TickTime(bars * BAR),
            ClipContent::Audio(AudioClip::new(format!("audio/{file}"), SampleTime(offset))),
        ))
    };
    let mut drums = Track::new(TrackKind::Audio, "Bass");
    drums.color = Some(Color(0x3d, 0xae, 0xe9));
    drums.clips = vec![
        audio_clip("Bass", "bass.wav", 0, 4, 0),
        audio_clip("Bass", "bass.wav", 4, 2, 0),
        audio_clip("Bass", "bass.wav", 8, 4, 0),
    ];
    // A swell in, then a fade out, so the screenshots show a lane.
    let mut swell = AutomationLane::new(AutomationTarget::Volume);
    let point = |bar: i64, db: f64, curve: f64| AutomationPoint {
        curve,
        ..AutomationPoint::new(TickTime(bar * BAR), db)
    };
    swell.points = vec![
        point(0, -24.0, 1.5),
        point(4, 0.0, 0.0),
        point(10, 0.0, -1.5),
        point(12, -40.0, 0.0),
    ];
    drums.automation.push(swell);
    let mut leads = Track::new(TrackKind::Audio, "Lead");
    leads.color = Some(Color(0xf6, 0x74, 0x00));
    // Faded and quieter, in view, so the screenshots show fades and clip gain.
    let mut lead = audio_clip("Lead", "lead.wav", 2, 2, 0);
    if let ClipContent::Audio(a) = &mut Arc::make_mut(&mut lead).content {
        a.gain_db = -4.0;
        a.fade_in = Fade::new(TickTime(BAR / 2), FadeShape::EqualPower);
        a.fade_out = Fade::new(TickTime(BAR), FadeShape::SCurve);
    }
    leads.clips = vec![lead, audio_clip("Lead take 2", "lead.wav", 6, 2, 12_000)];
    let mut keys = Track::new(TrackKind::Instrument, "Keys");
    keys.color = Some(Color(0x9b, 0x59, 0xb6));
    keys.devices.push(Device::new(
        "Synth",
        DeviceSource::Builtin {
            kind: "daw.synth".into(),
        },
    ));
    let mut pan = AutomationLane::new(AutomationTarget::Pan);
    pan.points = vec![
        AutomationPoint::new(TickTime(4 * BAR), -0.6),
        AutomationPoint::new(TickTime(5 * BAR), 0.6),
        AutomationPoint::new(TickTime(6 * BAR), -0.6),
    ];
    keys.automation.push(pan);
    let mut m = MidiClip::default();
    for (i, key) in [60u8, 64, 67, 72, 67, 64, 62, 65, 69, 74, 69, 65]
        .into_iter()
        .enumerate()
    {
        m.notes.push(Note::new(
            TickTime(i as i64 * PPQ / 2),
            TickTime(PPQ / 2),
            key,
            0.8,
        ));
    }
    keys.clips = vec![Arc::new(Clip::new(
        "Chords",
        TickTime(4 * BAR),
        TickTime(2 * BAR),
        ClipContent::Midi(m),
    ))];
    let mut reverb = Track::new(TrackKind::Bus, "Reverb");
    reverb.devices.push(Device::new(
        "Reverb",
        DeviceSource::Builtin {
            kind: "daw.reverb".into(),
        },
    ));
    let mut send = Send::new(reverb.id);
    send.level_db = -8.0;
    leads.sends.push(send);
    project.tracks = vec![
        Arc::new(drums),
        Arc::new(leads),
        Arc::new(keys),
        Arc::new(reverb),
    ];
    let mut doc = Document::new(project);
    doc.save_as(&dir).map_err(|e| e.to_string())?;
    Ok(doc)
}

/// A large project for `--benchmark`: `tracks` audio tracks of `clips`
/// one-beat clips each, all playing one short tone.
pub fn stress(tracks: usize, clips: usize) -> Result<Document, String> {
    let dir = std::env::temp_dir()
        .join(format!("kadence-benchmark-{}", std::process::id()))
        .join("Stress");
    let _ = std::fs::remove_dir_all(&dir);
    let audio = dir.join("audio");
    std::fs::create_dir_all(&audio).map_err(|e| e.to_string())?;
    write_wav(audio.join("tone.wav"), &pluck(220.0, 1), WavFormat::Float32)
        .map_err(|e| e.to_string())?;
    let mut project = Project::new("Stress");
    project.tracks = (0..tracks)
        .map(|t| {
            let mut track = Track::new(TrackKind::Audio, format!("Track {}", t + 1));
            track.clips = (0..clips)
                .map(|c| {
                    Arc::new(Clip::new(
                        format!("Clip {}", c + 1),
                        TickTime((2 * c as i64 + (t % 2) as i64) * PPQ),
                        TickTime(PPQ),
                        ClipContent::Audio(AudioClip::new("audio/tone.wav", SampleTime(0))),
                    ))
                })
                .collect();
            Arc::new(track)
        })
        .collect();
    let mut doc = Document::new(project);
    doc.save_as(&dir).map_err(|e| e.to_string())?;
    Ok(doc)
}

/// Removes what [`build`] and [`stress`] wrote.
pub fn remove_temp_files() {
    let pid = std::process::id();
    for name in [
        format!("kadence-demo-{pid}"),
        format!("kadence-benchmark-{pid}"),
    ] {
        let _ = std::fs::remove_dir_all(std::env::temp_dir().join(name));
    }
}
