//! The track list as the track headers show it, without naming Qt.
//!
//! [`TrackRow`] is one track beside the timeline: its name, colour and
//! switches, what it records from, its instrument, and its automation lanes
//! as their headers name them ([`LaneRow`]). [`AppState::track_rows`] reads
//! them all at once, so the list model over them
//! ([`Tracks`](crate::tracks_bridge)) can tell which rows changed. The edits
//! a header makes are here too, each one undo step.

use daw_arrange::Rgba;
use daw_model::commands::{MoveTrack, RemoveTrack, SetTrack, TrackField};
use daw_model::{
    AudioInput, InputChannels, MidiInput, Monitoring, Track, TrackId, TrackKind as ModelKind,
};

use crate::device_panel::ParamDesc;
use crate::instrument::{self, InstrumentChoice};
use crate::lanes::{LaneMode, TargetName, target_name};
use crate::state::AppState;

/// The kinds of track the list holds (the master is not in it).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum TrackKind {
    Audio,
    Instrument,
    Bus,
}

impl TrackKind {
    /// `None` for the master, which has no row.
    pub fn of(kind: ModelKind) -> Option<Self> {
        match kind {
            ModelKind::Audio => Some(Self::Audio),
            ModelKind::Instrument => Some(Self::Instrument),
            ModelKind::Bus => Some(Self::Bus),
            ModelKind::Master => None,
        }
    }

    pub fn model(self) -> ModelKind {
        match self {
            Self::Audio => ModelKind::Audio,
            Self::Instrument => ModelKind::Instrument,
            Self::Bus => ModelKind::Bus,
        }
    }
}

/// The instrument in an instrument track's slot.
#[derive(Clone, Debug, PartialEq)]
pub struct InstrumentRow {
    pub name: String,
    /// The device's id, for the device panel.
    pub device: String,
    /// Its index among the app's instrument choices.
    pub choice: Option<usize>,
}

/// One automation lane, as its header names it.
#[derive(Clone, Debug, PartialEq)]
pub struct LaneRow {
    pub name: TargetName,
    pub mode: LaneMode,
    /// How a device parameter's value is worded; `None` for other lanes
    /// and for parameters nothing describes.
    pub param: Option<ParamDesc>,
}

/// One track, as its header shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct TrackRow {
    pub id: TrackId,
    pub name: String,
    pub kind: TrackKind,
    pub color: Rgba,
    pub muted: bool,
    pub soloed: bool,
    /// Armed for recording.
    pub armed: bool,
    /// The hardware inputs an audio track records, and whether it monitors
    /// them while armed.
    pub audio_input: AudioInput,
    /// The live MIDI an instrument track listens to.
    pub midi_input: MidiInput,
    pub instrument: Option<InstrumentRow>,
    /// Whether the track shows its automation lanes.
    pub expanded: bool,
    pub lanes: Vec<LaneRow>,
}

impl AppState {
    /// Every track in list order.
    pub fn track_rows(&self) -> Vec<TrackRow> {
        let project = self.doc.project();
        project
            .tracks
            .iter()
            .enumerate()
            .filter_map(|(row, t)| {
                let instrument = instrument::instrument_of(t).map(|d| InstrumentRow {
                    name: d.name.clone(),
                    device: d.id.to_string(),
                    choice: self.instruments.position(&d.source),
                });
                Some(TrackRow {
                    id: t.id,
                    name: t.name.clone(),
                    kind: TrackKind::of(t.kind)?,
                    color: daw_arrange::track_color(t, row),
                    muted: t.mute,
                    soloed: t.solo,
                    armed: t.armed,
                    audio_input: t.audio_input,
                    midi_input: t.midi_input.clone(),
                    instrument,
                    expanded: self.arrange.expanded.contains(&t.id),
                    lanes: self.lane_rows(row),
                })
            })
            .collect()
    }

    /// The automation lanes of the track at `row`, in lane order; none for
    /// no such track.
    pub fn lane_rows(&self, row: usize) -> Vec<LaneRow> {
        let project = self.doc.project();
        let Some(t) = project.tracks.get(row) else {
            return Vec::new();
        };
        let lanes = t.automation.iter().enumerate();
        lanes
            .map(|(i, l)| LaneRow {
                name: target_name(project, &self.lanes.params, t, l.target),
                mode: self.lane_mode(row as i32, i as i32),
                param: self.lane_param(row as i32, i as i32),
            })
            .collect()
    }

    /// Hardware inputs the running audio engine has.
    pub fn input_count(&self) -> u16 {
        self.engine.session().map_or(0, |s| s.config().inputs)
    }

    fn track_id(&self, row: usize) -> Option<TrackId> {
        self.doc.project().tracks.get(row).map(|t| t.id)
    }

    /// The number for the next default track name of `kind`, as in
    /// "Audio 3".
    pub fn next_track_number(&self, kind: TrackKind) -> usize {
        let kind = kind.model();
        let tracks = &self.doc.project().tracks;
        tracks.iter().filter(|t| t.kind == kind).count() + 1
    }

    /// Adds a track after the selected one (or at the end) and selects it.
    /// An instrument track starts with the default instrument.
    pub fn add_track(&mut self, kind: TrackKind, name: String) {
        let p = self.doc.project();
        let at = self
            .arrange
            .selected_track
            .and_then(|id| p.track_index(id))
            .map_or(p.tracks.len(), |i| i + 1);
        let track = Track::new(kind.model(), name);
        let id = track.id;
        let command = instrument::add_track(at, track, self.instruments.default_choice());
        match self.doc.execute_boxed(command) {
            Ok(_) => self.arrange.selected_track = Some(id),
            Err(e) => self.report(e.into()),
        }
        self.doc.seal();
    }

    pub fn remove_track(&mut self, row: usize) {
        let Some(id) = self.track_id(row) else {
            return;
        };
        if let Err(e) = self.doc.execute(RemoveTrack::new(id)) {
            self.report(e.into());
        }
        self.doc.seal();
        if self.arrange.selected_track == Some(id) {
            self.arrange.selected_track = None;
        }
    }

    /// Moves the track at `from` to row `to`.
    pub fn move_track(&mut self, from: usize, to: usize) {
        let Some(id) = self.track_id(from) else {
            return;
        };
        if to >= self.doc.project().tracks.len() || to == from {
            return;
        }
        if let Err(e) = self.doc.execute(MoveTrack::new(id, to)) {
            self.report(e.into());
        }
        self.doc.seal();
    }

    /// Sets one field of the track at `row`.
    pub fn set_track_field(&mut self, row: usize, field: TrackField) {
        let Some(id) = self.track_id(row) else {
            return;
        };
        if let Err(e) = self.doc.execute(SetTrack::new(id, field)) {
            self.report(e.into());
        }
        self.doc.seal();
    }

    /// Renames the track at `row`; a blank name is ignored.
    pub fn rename_track(&mut self, row: usize, name: String) {
        if !name.trim().is_empty() {
            self.set_track_field(row, TrackField::Name(name));
        }
    }

    /// Changes the audio input of the track at `row` with `change`, unless
    /// that leaves it as it was.
    fn change_audio_input(&mut self, row: usize, change: impl FnOnce(&mut AudioInput)) {
        let Some(before) = self.doc.project().tracks.get(row).map(|t| t.audio_input) else {
            return;
        };
        let mut input = before;
        change(&mut input);
        if input != before {
            self.set_track_field(row, TrackField::AudioInput(input));
        }
    }

    /// Sets which hardware inputs an audio track records: `first` alone, or
    /// the stereo pair from `first` on.
    pub fn set_track_input(&mut self, row: usize, first: u16, stereo: bool) {
        self.change_audio_input(row, |input| {
            input.channels = if stereo {
                InputChannels::Stereo(first)
            } else {
                InputChannels::Mono(first)
            };
        });
    }

    /// Sets whether an audio track monitors its input while armed.
    pub fn set_track_monitor_auto(&mut self, row: usize, on: bool) {
        self.change_audio_input(row, |input| {
            input.monitor = if on {
                Monitoring::Auto
            } else {
                Monitoring::Off
            };
        });
    }

    /// Sets which live MIDI an instrument track takes.
    pub fn set_track_midi_input(&mut self, row: usize, input: MidiInput) {
        let same = |t: &std::sync::Arc<Track>| t.midi_input == input;
        if self.doc.project().tracks.get(row).is_none_or(same) {
            return;
        }
        self.set_track_field(row, TrackField::MidiInput(input));
    }

    /// Swaps the instrument of the track at `row` for `choice`.
    pub fn set_track_instrument(&mut self, row: usize, choice: &InstrumentChoice) {
        let Some(id) = self.track_id(row) else {
            return;
        };
        let Some(command) = instrument::set_instrument(self.doc.project(), id, choice) else {
            return;
        };
        if let Err(e) = self.doc.execute_boxed(command) {
            self.report(e.into());
        }
        self.doc.seal();
    }
}
