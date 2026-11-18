//! The track list as the track headers show it, without naming Qt.
//!
//! [`TrackRow`] is one track beside the timeline: its name, colour and
//! switches, what it records from, its instrument, and the rows under it
//! as their headers show them ([`HeaderRow`]). [`AppState::track_rows`] reads
//! them all at once, so the list model over them
//! ([`Tracks`](crate::tracks_bridge)) can tell which rows changed. The edits
//! a header makes are here too, each one undo step.

use daw_arrange::Rgba;
use daw_arrange::rows::{Group, RowKind};
use daw_model::commands::{MoveTrack, RemoveTrack, SetTrack, TrackField};
use daw_model::{
    AudioInput, AutomationTarget, InputChannels, MidiInput, Monitoring, Track, TrackId,
    TrackKind as ModelKind,
};

use crate::device_panel::ParamDesc;
use crate::instrument::{self, InstrumentChoice};
use crate::lanes::{LaneKind, LaneMode, TargetName, group_name, target_name};
use crate::session_mixer;
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

/// One row under a track, as its header shows it: a control, with or
/// without an automation lane, or the heading of a group of them (see
/// [`daw_arrange::rows`]).
#[derive(Clone, Debug, PartialEq)]
pub struct HeaderRow {
    /// What it is. A parameter under its device's heading goes by its own
    /// name alone (no `detail`).
    pub name: TargetName,
    /// How many groups it is inside.
    pub depth: u8,
    /// Whether the group it is the heading of is open.
    pub open: bool,
    /// The control's lane, as its index among the track's lanes.
    pub lane: Option<usize>,
    /// The lane's mode; a control without a lane reads.
    pub mode: LaneMode,
    /// How a device parameter's value is worded; `None` for other rows
    /// and for parameters nothing describes.
    pub param: Option<ParamDesc>,
    /// What the row's actions address in the Mixer: an effect's heading
    /// its place among the strip's inserts, a send's level its place
    /// among the strip's sends.
    pub item: Option<usize>,
}

/// One track, as its header shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct TrackRow {
    pub id: TrackId,
    /// Whether it is the selected track.
    pub selected: bool,
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
    /// The track's strip among the Mixer's rows.
    pub strip: Option<usize>,
    /// Whether the track shows the rows under it.
    pub expanded: bool,
    /// The rows under it while it is expanded.
    pub rows: Vec<HeaderRow>,
}

impl AppState {
    /// Every track in list order. A row's place here is its track's place
    /// in the project, which the edits below take: the master is never
    /// among a project's tracks.
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
                let expanded = self.arrange.expanded.contains(&t.id);
                Some(TrackRow {
                    id: t.id,
                    selected: self.arrange.selected_track == Some(t.id),
                    name: t.name.clone(),
                    kind: TrackKind::of(t.kind)?,
                    color: daw_arrange::track_color(t, row),
                    muted: t.mute,
                    soloed: t.solo,
                    armed: t.armed,
                    audio_input: t.audio_input,
                    midi_input: t.midi_input.clone(),
                    instrument,
                    strip: session_mixer::strip_row(project, t.id),
                    expanded,
                    rows: if expanded {
                        self.header_rows(row)
                    } else {
                        Vec::new()
                    },
                })
            })
            .collect()
    }

    /// The rows the track at `row` shows under it when expanded, as the
    /// timeline lays them out; none for no such track.
    pub fn header_rows(&self, row: usize) -> Vec<HeaderRow> {
        let Some(t) = self.doc.project().tracks.get(row) else {
            return Vec::new();
        };
        let rows = self.arrange.rows_of(t);
        let all = rows.iter().enumerate();
        all.map(|(i, r)| match r.kind {
            RowKind::Group(g) => HeaderRow {
                name: group_name(t, g),
                depth: r.depth,
                open: r.open,
                lane: None,
                mode: LaneMode::Read,
                param: None,
                item: match g {
                    Group::Device(id) => {
                        let at = t.devices.iter().position(|d| d.id == id);
                        at.and_then(|i| i.checked_sub(instrument::effects_start(t)))
                    }
                    _ => None,
                },
            },
            RowKind::Control(target) => {
                // The heading the row is under: the nearest shallower row.
                let heading = rows[..i].iter().rev().find(|x| x.depth < r.depth);
                let named = matches!(
                    heading.and_then(daw_arrange::LaneRow::group),
                    Some(Group::Device(_) | Group::Instrument)
                );
                HeaderRow {
                    depth: r.depth,
                    ..self.control_row(row, t, target, named)
                }
            }
        })
        .collect()
    }

    /// The automation lanes of the track at `row` alone, in lane order;
    /// none for no such track.
    pub fn lane_rows(&self, row: usize) -> Vec<HeaderRow> {
        let Some(t) = self.doc.project().tracks.get(row) else {
            return Vec::new();
        };
        let lanes = t.automation.iter();
        lanes
            .map(|l| self.control_row(row, t, l.target, false))
            .collect()
    }

    /// The row of the control `target` of track `t` (at `row`), outside
    /// any group. `under_device`: it is listed under its device's heading,
    /// which names the device for it.
    fn control_row(
        &self,
        row: usize,
        t: &Track,
        target: AutomationTarget,
        under_device: bool,
    ) -> HeaderRow {
        let mut name = target_name(self.doc.project(), &self.lanes.params, t, target);
        if under_device && name.kind == LaneKind::DeviceParam {
            name.detail.clear();
        }
        let lane = t.automation.iter().position(|l| l.target == target);
        HeaderRow {
            name,
            depth: 0,
            open: false,
            lane,
            mode: lane.map_or(LaneMode::Read, |l| self.lane_mode(row as i32, l as i32)),
            param: self.target_param(row as i32, target),
            item: match target {
                AutomationTarget::SendLevel { send } => t.sends.iter().position(|s| s.id == send),
                _ => None,
            },
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::StubEngine;

    fn state() -> AppState {
        AppState::with_engine(Box::new(StubEngine::default()))
    }

    fn names(s: &AppState) -> Vec<String> {
        s.track_rows().into_iter().map(|t| t.name).collect()
    }

    #[test]
    fn a_new_track_follows_the_selected_one_and_is_selected() {
        let mut s = state();
        s.add_track(TrackKind::Audio, "A".into());
        s.add_track(TrackKind::Bus, "B".into());
        s.arrange.selected_track = Some(s.track_rows()[0].id);
        s.add_track(TrackKind::Instrument, "C".into());

        assert_eq!(names(&s), ["A", "C", "B"]);
        let rows = s.track_rows();
        let selected: Vec<bool> = rows.iter().map(|t| t.selected).collect();
        assert_eq!(selected, [false, true, false]);
        assert_eq!(rows[1].kind, TrackKind::Instrument);
        assert!(rows[1].instrument.is_some(), "the default instrument");
        assert!(rows[0].instrument.is_none());
        assert_eq!(s.next_track_number(TrackKind::Audio), 2);
    }

    #[test]
    fn removing_the_selected_track_clears_the_selection() {
        let mut s = state();
        s.add_track(TrackKind::Audio, "A".into());
        s.add_track(TrackKind::Audio, "B".into());
        s.remove_track(1);
        assert_eq!(names(&s), ["A"]);
        assert_eq!(s.arrange.selected_track, None);
        // No such row: nothing happens.
        s.remove_track(5);
        assert_eq!(names(&s), ["A"]);
    }

    #[test]
    fn moves_stay_inside_the_list() {
        let mut s = state();
        for name in ["A", "B", "C"] {
            s.add_track(TrackKind::Audio, name.into());
        }
        s.move_track(0, 2);
        assert_eq!(names(&s), ["B", "C", "A"]);
        let steps = s.doc.revision();
        s.move_track(0, 3);
        s.move_track(1, 1);
        s.move_track(7, 0);
        assert_eq!(names(&s), ["B", "C", "A"]);
        assert_eq!(s.doc.revision(), steps, "no undo step for a move nowhere");
    }

    #[test]
    fn settings_that_change_nothing_are_not_undo_steps() {
        let mut s = state();
        s.add_track(TrackKind::Audio, "A".into());
        s.rename_track(0, "  ".into());
        assert_eq!(names(&s), ["A"]);

        s.set_track_input(0, 2, true);
        assert_eq!(
            s.track_rows()[0].audio_input.channels,
            InputChannels::Stereo(2)
        );
        let steps = s.doc.revision();
        s.set_track_input(0, 2, true);
        let monitor = s.track_rows()[0].audio_input.monitor == Monitoring::Auto;
        s.set_track_monitor_auto(0, monitor);
        s.set_track_midi_input(0, MidiInput::default());
        assert_eq!(s.doc.revision(), steps);

        s.set_track_monitor_auto(0, !monitor);
        assert_ne!(s.doc.revision(), steps);
    }
}
