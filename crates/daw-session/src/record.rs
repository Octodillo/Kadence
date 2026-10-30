//! Recording audio onto armed tracks.
//!
//! [`AudioTakes::start`] opens one [`Recorder`] per armed audio track and
//! hands its input to the track's [`InputTap`](crate::capture::InputTap).
//! [`AudioTakes::finish`] stops them and returns one undoable command that
//! adds every take as clips.
//!
//! # Lining takes up
//!
//! A sample the tap records at transport position `p` was played by the
//! performer `latency` samples earlier, as they heard the project:
//!
//! * the backend's output latency: position `q` leaves the speakers
//!   `output` samples after the engine produced it;
//! * the graph's latency: plugin delay compensation makes the master play
//!   position `q - graph` at `q`;
//! * the backend's input latency: what reaches the microphone arrives
//!   `input` samples later.
//!
//! So the take's first frame belongs at `p - (input + output + graph)`.
//! Clips start on whole ticks; the clip starts on the first tick at or
//! after that point and skips the frames before it with its file offset,
//! which keeps every sample where it belongs. A take longer than one WAV
//! file holds (hours) becomes one clip per file; up to a tick of audio at
//! each file boundary falls between the two clips.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use daw_core::time::{SampleTime, TickTime};
use daw_media::{MediaError, Recorder, WavFormat};
use daw_model::commands::{AddClip, Batch};
use daw_model::{
    AudioClip, Clip, ClipContent, Command, InputChannels, Project, TrackId, TrackKind,
};

use crate::Session;
use crate::capture::TakeProgress;

/// How long the disk may stall before recorded frames are dropped.
const BUFFER_SECONDS: f32 = 4.0;

/// Something that went wrong with a take. The variants carry data so a
/// front end can word them in the user's language.
#[derive(Clone, Debug, thiserror::Error)]
pub enum TakeProblem {
    /// The take's file could not be created.
    #[error("can't record to {}: {detail}", path.display())]
    CannotCreate { path: PathBuf, detail: String },
    /// The track has no input tap (the engine has no inputs, or the graph
    /// isn't built yet).
    #[error("track {track} can't record yet")]
    NotReady { track: String },
    /// The track's inputs don't exist on the running backend.
    #[error("track {track} records inputs the audio system doesn't have")]
    NoSuchInput { track: String },
    /// Writing stopped early (disk full, drive removed). What was written
    /// before is kept.
    #[error("recording {track} stopped: {error}")]
    WriteFailed {
        track: String,
        error: Arc<MediaError>,
    },
    /// The disk fell behind and some audio was lost; later audio in the
    /// take is early by that much.
    #[error("{frames} frames of {track} were lost")]
    Dropped { track: String, frames: u64 },
    /// The track was deleted while recording; its take is thrown away.
    #[error("track {track} was deleted while recording")]
    TrackGone { track: String },
    /// The transport jumped (loop wrap, locate) while recording; the take
    /// runs straight on, so what came after the jump is placed after the
    /// part before it.
    #[error("the play position jumped while recording {track}")]
    Jumped { track: String },
}

struct Take {
    track: TrackId,
    name: String,
    recorder: Recorder,
    progress: TakeProgress,
    /// Reported so far, so each problem shows once.
    failed: bool,
    dropped: u64,
}

/// Takes in progress. See the module docs.
pub struct AudioTakes {
    takes: Vec<Take>,
    /// The backend's round trip, as last reported.
    io_latency: i64,
    /// The graph's latency when recording started.
    graph_latency: i64,
}

/// What [`AudioTakes::finish`] produced.
pub struct Finished {
    /// Adds the takes as clips, as one undo step. `None` when nothing was
    /// recorded.
    pub command: Option<Box<dyn Command>>,
    pub problems: Vec<TakeProblem>,
}

/// The audio tracks armed in `project`, in track order.
pub fn armed_audio_tracks(project: &Project) -> impl Iterator<Item = TrackId> + '_ {
    project
        .tracks
        .iter()
        .filter(|t| t.armed && t.kind == TrackKind::Audio)
        .map(|t| t.id)
}

impl AudioTakes {
    /// Starts recording every armed audio track of `project` into new WAV
    /// files in `dir`. `io_latency` is the backend's round trip (input plus
    /// output latency); the graph's own latency is added here. Recording
    /// begins with the next block the transport plays.
    ///
    /// Tracks that can't record are reported and left out; the rest record.
    pub fn start(
        session: &mut Session,
        project: &Project,
        dir: &Path,
        io_latency: u32,
    ) -> (Self, Vec<TakeProblem>) {
        let io_latency = i64::from(io_latency);
        let graph_latency = i64::from(session.controller().latency());
        let mut takes = Vec::new();
        let mut problems = Vec::new();
        let rate = session.sample_rate().round() as u32;
        let inputs = session.config().inputs;
        if let Err(e) = std::fs::create_dir_all(dir) {
            problems.push(TakeProblem::CannotCreate {
                path: dir.to_owned(),
                detail: e.to_string(),
            });
            return (
                Self {
                    takes,
                    io_latency,
                    graph_latency,
                },
                problems,
            );
        }
        for id in armed_audio_tracks(project).collect::<Vec<_>>() {
            let track = project.track(id).expect("listed above");
            let channels: InputChannels = track.audio_input.channels;
            if u32::from(channels.first()) + u32::from(channels.count()) > u32::from(inputs) {
                problems.push(TakeProblem::NoSuchInput {
                    track: track.name.clone(),
                });
                continue;
            }
            if session.tap_mut(id).is_none() {
                problems.push(TakeProblem::NotReady {
                    track: track.name.clone(),
                });
                continue;
            }
            let path = free_path(dir, &track.name);
            let (recorder, input) = match Recorder::start(
                &path,
                channels.count() as usize,
                rate,
                WavFormat::Float32,
                BUFFER_SECONDS,
            ) {
                Ok(r) => r,
                Err(e) => {
                    problems.push(TakeProblem::CannotCreate {
                        path,
                        detail: e.to_string(),
                    });
                    continue;
                }
            };
            let tap = session.tap_mut(id).expect("checked above");
            if tap.start(input, channels).is_err() {
                problems.push(TakeProblem::NotReady {
                    track: track.name.clone(),
                });
                drop(recorder.finish());
                let _ = std::fs::remove_file(&path);
                continue;
            }
            let progress = tap.progress();
            takes.push(Take {
                track: id,
                name: track.name.clone(),
                recorder,
                progress,
                failed: false,
                dropped: 0,
            });
        }
        session.update_monitors();
        (
            Self {
                takes,
                io_latency,
                graph_latency,
            },
            problems,
        )
    }

    /// Whether no track is recording.
    pub fn is_empty(&self) -> bool {
        self.takes.is_empty()
    }

    /// The tracks being recorded.
    pub fn tracks(&self) -> impl Iterator<Item = TrackId> + '_ {
        self.takes.iter().map(|t| t.track)
    }

    /// The round trip latency the takes are compensated by, in samples.
    pub fn latency(&self) -> i64 {
        self.io_latency + self.graph_latency
    }

    /// Updates the backend's round trip before [`finish`](Self::finish).
    /// JACK (PipeWire) recomputes port latencies a while after the server
    /// starts or connections change, and can report 0 meanwhile, so the
    /// value read when recording stopped is the better one.
    pub fn set_io_latency(&mut self, io_latency: u32) {
        self.io_latency = i64::from(io_latency);
    }

    /// Problems that turned up since the last call, each reported once.
    pub fn poll(&mut self) -> Vec<TakeProblem> {
        let mut out = Vec::new();
        for t in &mut self.takes {
            if t.recorder.failed() && !t.failed {
                t.failed = true;
                // The error itself comes with `finish`.
                out.push(TakeProblem::WriteFailed {
                    track: t.name.clone(),
                    error: Arc::new(MediaError::Encode("the disk refused the recording".into())),
                });
            }
            let dropped = t.recorder.dropped_frames();
            if dropped > 0 && t.dropped == 0 {
                out.push(TakeProblem::Dropped {
                    track: t.name.clone(),
                    frames: dropped,
                });
            }
            t.dropped = dropped;
        }
        out
    }

    /// Stops recording and returns the command that adds the takes. Clip
    /// sources are relative to `root` when inside it. Files that hold no
    /// usable audio are deleted.
    pub fn finish(self, session: &mut Session, project: &Project, root: Option<&Path>) -> Finished {
        let mut problems = Vec::new();
        let mut commands: Vec<Box<dyn Command>> = Vec::new();
        let latency = self.latency();
        for t in self.takes {
            if let Some(tap) = session.tap_mut(t.track) {
                // A full queue only happens if stop is called in a burst; a
                // few tries are plenty. If it still fails, the tap writes
                // into a recorder nobody drains, which is harmless.
                for _ in 0..3 {
                    if tap.stop() {
                        break;
                    }
                }
            }
            // Read from the take's own record: if the track was deleted and
            // brought back by undo, its tap is a new one, but the audio
            // recorded until the deletion still belongs where it started.
            let (started, jumped) = (t.progress.started_at(), t.progress.jumped());
            let stats = match t.recorder.finish() {
                Ok(s) => s,
                Err(e) => {
                    problems.push(TakeProblem::WriteFailed {
                        track: t.name.clone(),
                        error: Arc::new(e),
                    });
                    continue;
                }
            };
            if let Some(error) = &stats.error {
                problems.push(TakeProblem::WriteFailed {
                    track: t.name.clone(),
                    error: error.clone(),
                });
            }
            if stats.dropped_frames > 0 && t.dropped == 0 {
                problems.push(TakeProblem::Dropped {
                    track: t.name.clone(),
                    frames: stats.dropped_frames,
                });
            }
            if jumped {
                problems.push(TakeProblem::Jumped {
                    track: t.name.clone(),
                });
            }
            let alive = project.track(t.track).is_some();
            if !alive && started.is_some() {
                problems.push(TakeProblem::TrackGone {
                    track: t.name.clone(),
                });
            }
            let mut at = started.map(|s| s - latency);
            for file in &stats.files {
                let placed = at.and_then(|t0| place(session, t0, file.frames));
                match placed {
                    Some((start, length, offset)) if alive => {
                        let source = match root.and_then(|r| file.path.strip_prefix(r).ok()) {
                            Some(rel) => rel.to_path_buf(),
                            None => file.path.clone(),
                        };
                        let name = file
                            .path
                            .file_stem()
                            .map(|s| s.to_string_lossy().into_owned())
                            .unwrap_or_else(|| t.name.clone());
                        let clip = Clip::new(
                            name,
                            start,
                            length,
                            ClipContent::Audio(AudioClip::new(source, offset)),
                        );
                        commands.push(Box::new(AddClip::new(t.track, clip)));
                    }
                    _ => remove_take_file(&file.path),
                }
                at = at.map(|a| a + file.frames as i64);
            }
        }
        session.update_monitors();
        Finished {
            command: (!commands.is_empty())
                .then(|| Box::new(Batch::new("Record Audio", commands)) as Box<dyn Command>),
            problems,
        }
    }

    /// Stops recording and deletes the takes.
    pub fn discard(self, session: &mut Session) {
        for t in self.takes {
            if let Some(tap) = session.tap_mut(t.track) {
                tap.stop();
            }
            if let Ok(stats) = t.recorder.finish() {
                for f in &stats.files {
                    remove_take_file(&f.path);
                }
            }
        }
        session.update_monitors();
    }
}

/// Where a recorded file of `frames` frames whose first frame belongs at
/// timeline sample `t0` goes: clip start, length and file offset. `None`
/// when nothing of it lands at or after the project start on a whole tick.
pub fn place(session: &Session, t0: i64, frames: u64) -> Option<(TickTime, TickTime, SampleTime)> {
    let to_samples = |t: i64| session.ticks_to_samples(TickTime(t)).0;
    let from = t0.max(0);
    // The first tick at or after `from`.
    let mut start = session.samples_to_ticks(SampleTime(from)).0;
    while to_samples(start) < from {
        start += 1;
    }
    while start > 0 && to_samples(start - 1) >= from {
        start -= 1;
    }
    let end_sample = t0 + frames as i64;
    // The last tick at or before the file's end.
    let mut end = session.samples_to_ticks(SampleTime(end_sample)).0;
    while to_samples(end) > end_sample {
        end -= 1;
    }
    while to_samples(end + 1) <= end_sample {
        end += 1;
    }
    let offset = to_samples(start) - t0;
    (end > start && offset >= 0).then_some((
        TickTime(start),
        TickTime(end - start),
        SampleTime(offset),
    ))
}

/// A file name in `dir` for a new take on a track called `track`.
fn free_path(dir: &Path, track: &str) -> PathBuf {
    let stem: String = track
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' => '-',
            c if c.is_control() => '-',
            c => c,
        })
        .collect();
    let stem = stem.trim().trim_start_matches('.');
    let stem = if stem.is_empty() { "audio" } else { stem };
    (1..)
        .map(|n| dir.join(format!("{stem} take {n}.wav")))
        // A long take continues in "<name>-2.wav"; keep those free too.
        .find(|p| !p.exists() && !with_suffix(p, 2).exists())
        .expect("ran out of names")
}

fn with_suffix(path: &Path, n: u32) -> PathBuf {
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    path.with_file_name(format!("{stem}-{n}.wav"))
}

/// Deletes a take's file and its peak cache.
fn remove_take_file(path: &Path) {
    let _ = std::fs::remove_file(path);
    let mut peaks = path.as_os_str().to_owned();
    peaks.push(".peaks");
    let _ = std::fs::remove_file(PathBuf::from(peaks));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SessionConfig;

    #[test]
    fn placing_keeps_every_frame_where_it_belongs() {
        let (session, _proc) = Session::new(SessionConfig::default());
        let s = |t: TickTime| session.ticks_to_samples(t).0;
        // At 120 bpm and 48 kHz a tick is exactly 25 samples.
        for t0 in [-100, -1, 0, 1, 24, 25, 26, 1_000_003] {
            let (start, length, offset) = place(&session, t0, 10_000).unwrap();
            assert!(start.0 >= 0 && length.0 > 0);
            // File frame `offset` plays at the clip start, so frame 0 is at t0.
            assert_eq!(s(start) - offset.0, t0, "t0 {t0}");
            assert!(offset.0 >= 0 && offset.0 < 25 + (-t0).max(0));
            // The clip ends inside the file.
            assert!(s(TickTime(start.0 + length.0)) <= t0 + 10_000);
        }
        // Nothing left after the project start.
        assert_eq!(place(&session, -20_000, 10_000), None);
    }

    #[test]
    fn take_names_are_safe_and_free() {
        let dir = tempfile::tempdir().unwrap();
        let p = free_path(dir.path(), "../Vox: 1");
        assert_eq!(p.file_name().unwrap(), "-Vox- 1 take 1.wav");
        std::fs::write(&p, b"").unwrap();
        std::fs::write(dir.path().join("-Vox- 1 take 2-2.wav"), b"").unwrap();
        assert_eq!(
            free_path(dir.path(), "../Vox: 1").file_name().unwrap(),
            "-Vox- 1 take 3.wav"
        );
        assert_eq!(
            free_path(dir.path(), " . ").file_name().unwrap(),
            "audio take 1.wav"
        );
    }
}
