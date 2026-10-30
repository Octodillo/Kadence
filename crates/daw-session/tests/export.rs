//! Exports small projects and checks the file against what playback plays.

use std::path::Path;

use daw_core::event::EventList;
use daw_core::time::{PPQ, SampleTime, TickTime};
use daw_engine::EngineConfig;
use daw_media::{AudioData, FileFormat, FlacBits, WavFormat, decode_file, write_wav};
use daw_model::commands::{AddClip, AddDevice, AddLane, AddTrack};
use daw_model::{
    AudioClip, AutomationLane, AutomationPoint, AutomationTarget, Clip, ClipContent, Device,
    DeviceSource, Document, MidiClip, Note, Project, ProjectSnapshot, Track, TrackKind,
};
use daw_session::export::{
    ExportError, ExportJob, ExportSettings, Progress, RenderSettings, export, render, song_end,
};
use daw_session::{Session, SessionConfig};

const RATE: u32 = 48_000;
/// Samples per beat at the default 120 bpm.
const BEAT: usize = 24_000;

/// A stereo file whose samples all differ, so a misplaced read shows.
fn ramp_file(path: &Path, frames: usize) {
    let l: Vec<f32> = (0..frames)
        .map(|i| ((i % 4801) as f32 / 4800.0 - 0.5) * 0.6)
        .collect();
    let r = l.iter().map(|x| -x * 0.5).collect();
    let data = AudioData {
        sample_rate: RATE,
        channels: vec![l, r],
    };
    write_wav(path, &data, WavFormat::Float32).unwrap();
}

/// A song with what playback has: a short clip played from memory, a clip of
/// a file long enough to be streamed, a synth playing notes, a compressor and
/// a volume lane.
fn song(dir: &Path) -> Document {
    ramp_file(&dir.join("short.wav"), 2 * BEAT);
    // Over the 60 s memory limit, so the export streams it.
    ramp_file(&dir.join("long.wav"), 62 * RATE as usize);
    let mut doc = Document::new(Project::new("song"));

    let audio = Track::new(TrackKind::Audio, "Audio");
    let audio_id = audio.id;
    doc.execute(AddTrack::new(0, audio)).unwrap();
    let clip = |file: &str, start: i64, beats: i64, offset: i64| {
        Clip::new(
            file,
            TickTime(start * PPQ),
            TickTime(beats * PPQ),
            ClipContent::Audio(AudioClip {
                source: file.into(),
                offset: SampleTime(offset),
                gain_db: -3.0,
            }),
        )
    };
    doc.execute(AddClip::new(audio_id, clip("short.wav", 0, 2, 0)))
        .unwrap();
    doc.execute(AddClip::new(audio_id, clip("long.wav", 3, 4, 1000)))
        .unwrap();
    doc.execute(AddDevice {
        track: audio_id,
        index: 0,
        device: Device::new(
            "Compressor",
            DeviceSource::Builtin {
                kind: "daw.compressor".into(),
            },
        ),
    })
    .unwrap();
    let mut lane = AutomationLane::new(AutomationTarget::Volume);
    lane.points = vec![
        AutomationPoint {
            tick: TickTime(0),
            value: 0.0,
            curve: 0.0,
            step: false,
        },
        AutomationPoint {
            tick: TickTime(6 * PPQ),
            value: -12.0,
            curve: 0.0,
            step: false,
        },
    ];
    doc.execute(AddLane {
        track: audio_id,
        lane,
    })
    .unwrap();

    let keys = Track::new(TrackKind::Instrument, "Keys");
    let keys_id = keys.id;
    doc.execute(AddTrack::new(1, keys)).unwrap();
    doc.execute(AddDevice {
        track: keys_id,
        index: 0,
        device: Device::new(
            "Synth",
            DeviceSource::Builtin {
                kind: "daw.synth".into(),
            },
        ),
    })
    .unwrap();
    let mut notes = MidiClip::default();
    for (i, key) in [60u8, 64, 67, 72, 67, 64].into_iter().enumerate() {
        notes.notes.push(Note::new(
            TickTime(i as i64 * PPQ / 2),
            TickTime(PPQ / 2),
            key,
            0.8,
        ));
    }
    doc.execute(AddClip::new(
        keys_id,
        Clip::new(
            "notes",
            TickTime(PPQ),
            TickTime(4 * PPQ),
            ClipContent::Midi(notes),
        ),
    ))
    .unwrap();
    doc
}

/// What playback plays from `start`, `frames` long, interleaved: the song
/// through a session as the audio host runs it, in 256-frame periods, with
/// every file in memory so nothing can underrun.
fn playback(snapshot: &ProjectSnapshot, dir: &Path, start: TickTime, frames: usize) -> Vec<f32> {
    let (mut session, mut proc) = Session::new(SessionConfig {
        engine: EngineConfig {
            sample_rate: f64::from(RATE),
            max_frames: 1024,
            ..Default::default()
        },
        inputs: 0,
        outputs: 2,
    });
    session.media_mut().set_memory_limit(f64::INFINITY);
    session.set_root(Some(dir.to_path_buf()));
    session.sync(snapshot).unwrap();
    session.locate(start).unwrap();
    session.play().unwrap();
    let events = EventList::with_capacity(0);
    let mut l = vec![0.0; 256];
    let mut r = vec![0.0; 256];
    let mut out = Vec::with_capacity(frames * 2);
    while out.len() < frames * 2 {
        let n = 256.min(frames - out.len() / 2);
        let mut outs: [&mut [f32]; 2] = [&mut l[..n], &mut r[..n]];
        proc.process(n, &[], &mut outs, &events);
        for (l, r) in l[..n].iter().zip(&r[..n]) {
            out.extend([*l, *r]);
        }
        session.collect_garbage();
    }
    out
}

/// Renders through `render` and returns the output interleaved.
fn rendered(snapshot: &ProjectSnapshot, dir: &Path, settings: &RenderSettings) -> Vec<f32> {
    let mut out = Vec::new();
    render(
        snapshot,
        Some(dir.to_path_buf()),
        settings,
        &Progress::default(),
        |planes| {
            for (l, r) in planes[0].iter().zip(planes[1]) {
                out.extend([*l, *r]);
            }
            Ok(())
        },
    )
    .unwrap();
    out
}

fn max_diff(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len());
    a.iter().zip(b).fold(0.0, |m, (x, y)| m.max((x - y).abs()))
}

fn peak(s: &[f32]) -> f32 {
    s.iter().fold(0.0, |m, v| m.max(v.abs()))
}

fn settings(start: i64, end: i64, tail: f64) -> RenderSettings {
    RenderSettings {
        start: TickTime(start),
        end: TickTime(end),
        tail_seconds: tail,
        sample_rate: RATE,
    }
}

#[test]
fn whole_song_export_matches_playback() {
    let dir = tempfile::tempdir().unwrap();
    let mut doc = song(dir.path());
    let snap = doc.snapshot();
    let end = song_end(&snap);
    assert_eq!(end, TickTime(7 * PPQ), "the long clip ends last");
    let frames = 7 * BEAT;
    let expected = playback(&snap, dir.path(), TickTime(0), frames);
    let got = rendered(&snap, dir.path(), &settings(0, end.0, 0.0));
    assert!(peak(&expected) > 0.1, "the song is not silent");
    // The streamed clip plays every sample: no dropouts against the
    // in-memory playback.
    let d = max_diff(&got, &expected);
    assert!(d < 1e-5, "export differs from playback by {d}");
}

#[test]
fn a_custom_range_matches_playback_from_its_start() {
    let dir = tempfile::tempdir().unwrap();
    let mut doc = song(dir.path());
    let snap = doc.snapshot();
    // From inside the streamed clip, so the export starts with a seek.
    let (start, end) = (4 * PPQ + PPQ / 3, 6 * PPQ);
    let frames = 2 * BEAT - BEAT / 3;
    let expected = playback(&snap, dir.path(), TickTime(start), frames);
    let got = rendered(&snap, dir.path(), &settings(start, end, 0.0));
    assert!(peak(&expected) > 0.02, "{}", peak(&expected));
    let d = max_diff(&got, &expected);
    assert!(d < 1e-5, "export differs from playback by {d}");
}

#[test]
fn the_tail_rings_on_after_clips_stop() {
    let dir = tempfile::tempdir().unwrap();
    let mut doc = song(dir.path());
    let snap = doc.snapshot();
    // End in the middle of a held note and of the streamed clip.
    let got = rendered(&snap, dir.path(), &settings(0, 3 * PPQ + PPQ / 4, 0.5));
    let body = (3 * BEAT + BEAT / 4) * 2;
    assert_eq!(got.len(), body + RATE as usize);
    let tail = &got[body..];
    // The synth's release sounds just after the end...
    assert!(peak(&tail[..2000]) > 1e-3, "release in the tail");
    // ...and dies away; the long clip, cut at the end, doesn't play on.
    assert!(peak(&tail[tail.len() - 2000..]) < 1e-3, "the tail fades");
}

#[test]
fn wav_and_flac_files_hold_the_render() {
    let dir = tempfile::tempdir().unwrap();
    let mut doc = song(dir.path());
    let snap = doc.snapshot();
    let render = settings(0, 2 * PPQ, 0.0);
    let reference = rendered(&snap, dir.path(), &render);
    let cases = [
        (FileFormat::Wav(WavFormat::Float32), false, 0.0),
        (FileFormat::Wav(WavFormat::Pcm24), false, 1.0 / 8_388_608.0),
        (FileFormat::Wav(WavFormat::Pcm16), true, 2.0 / 32_768.0),
        (FileFormat::Flac(FlacBits::B24), false, 1.0 / 8_388_608.0),
        (FileFormat::Flac(FlacBits::B16), true, 2.0 / 32_768.0),
    ];
    for (i, (format, dither, tolerance)) in cases.into_iter().enumerate() {
        let path = dir.path().join(format!("out{i}"));
        let report = export(
            &snap,
            Some(dir.path().to_path_buf()),
            &path,
            &ExportSettings {
                render: render.clone(),
                format,
                dither,
            },
            &Progress::default(),
        )
        .unwrap();
        assert_eq!(report.frames as usize, 2 * BEAT);
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
        let data = decode_file(&path).unwrap();
        assert_eq!(data.sample_rate, RATE);
        let got = data.interleaved();
        let d = max_diff(&got, &reference);
        assert!(
            d <= tolerance * 1.01,
            "{format:?} differs by {d} (allowed {tolerance})"
        );
    }
}

#[test]
fn cancelling_leaves_no_file_and_keeps_the_old_one() {
    let dir = tempfile::tempdir().unwrap();
    let mut doc = song(dir.path());
    let snap = doc.snapshot();
    let path = dir.path().join("mix.wav");
    std::fs::write(&path, b"an older mix").unwrap();
    let before: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
    let progress = Progress::default();
    let mut blocks = 0;
    let settings = ExportSettings {
        render: settings(0, 7 * PPQ, 2.0),
        format: FileFormat::Wav(WavFormat::Pcm24),
        dither: false,
    };
    // Cancel from inside the render, partway through.
    let r = render(
        &snap,
        Some(dir.path().to_path_buf()),
        &settings.render,
        &progress,
        |_| {
            blocks += 1;
            if blocks == 50 {
                progress.cancel();
            }
            Ok(())
        },
    );
    assert!(matches!(r, Err(ExportError::Cancelled)), "{r:?}");
    assert!(progress.fraction() > 0.0 && progress.fraction() < 1.0);

    // A job cancelled while it writes.
    let job = ExportJob::start(
        snap.clone(),
        Some(dir.path().to_path_buf()),
        path.clone(),
        settings,
    )
    .unwrap();
    while job.progress() == 0.0 && !job.is_finished() {
        std::thread::yield_now();
    }
    job.cancel();
    assert!(matches!(job.wait(), Err(ExportError::Cancelled)));
    assert_eq!(std::fs::read(&path).unwrap(), b"an older mix");
    let after: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
    assert_eq!(after.len(), before.len(), "no temporary file left");
}

#[test]
fn a_job_reports_progress_and_finishes() {
    let dir = tempfile::tempdir().unwrap();
    let mut doc = song(dir.path());
    let snap = doc.snapshot();
    let path = dir.path().join("mix.flac");
    let job = ExportJob::start(
        snap,
        Some(dir.path().to_path_buf()),
        path.clone(),
        ExportSettings {
            render: settings(0, 4 * PPQ, 0.25),
            format: FileFormat::Flac(FlacBits::B16),
            dither: true,
        },
    )
    .unwrap();
    assert_eq!(job.path(), path);
    while !job.is_finished() {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(job.progress(), 1.0);
    let report = job.wait().unwrap();
    assert_eq!(report.frames as usize, 4 * BEAT + RATE as usize / 4);
    assert_eq!(decode_file(&path).unwrap().frames(), report.frames as usize);
}

#[test]
fn bad_settings_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let mut doc = Document::new(Project::new("empty"));
    let snap = doc.snapshot();
    assert_eq!(song_end(&snap), TickTime(0));
    let run = |s: RenderSettings| render(&snap, None, &s, &Progress::default(), |_| Ok(()));
    assert!(matches!(
        run(settings(0, 0, 1.0)),
        Err(ExportError::EmptyRange)
    ));
    assert!(matches!(
        run(RenderSettings {
            sample_rate: 100,
            ..settings(0, PPQ, 0.0)
        }),
        Err(ExportError::SampleRate(100))
    ));
    assert!(matches!(
        run(settings(0, PPQ, f64::NAN)),
        Err(ExportError::Tail(_))
    ));
    // An empty project renders silence over a range.
    let path = dir.path().join("silence.wav");
    let report = export(
        &snap,
        None,
        &path,
        &ExportSettings {
            render: settings(0, PPQ, 0.0),
            format: FileFormat::Wav(WavFormat::Pcm16),
            dither: false,
        },
        &Progress::default(),
    )
    .unwrap();
    assert_eq!(report.frames as usize, BEAT);
    assert_eq!(peak(&decode_file(&path).unwrap().interleaved()), 0.0);
}

#[test]
fn other_sample_rates_render_the_same_length_of_time() {
    let dir = tempfile::tempdir().unwrap();
    let mut doc = song(dir.path());
    let snap = doc.snapshot();
    let got = rendered(
        &snap,
        dir.path(),
        &RenderSettings {
            sample_rate: 44_100,
            ..settings(0, 2 * PPQ, 0.0)
        },
    );
    assert_eq!(got.len(), 44_100 * 2);
    assert!(peak(&got) > 0.1);
}
