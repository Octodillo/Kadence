//! The model tree: plain data, edited only through commands.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use daw_core::event::ParamId;
use daw_core::plugin::PluginFormatKind;
use daw_core::time::{SampleTime, TickTime};
use serde::{Deserialize, Serialize};

use crate::error::ModelError;
use crate::id::{ClipId, DeviceId, LaneId, NoteUid, SendId, TrackId};
use crate::serde_util;

/// The whole document.
///
/// Tracks and clips sit behind `Arc`s so a [`crate::ProjectSnapshot`] shares
/// everything an edit didn't touch. Mutate them through `Arc::make_mut`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub name: String,
    /// Preferred sample rate; the engine may run at another one.
    pub sample_rate: u32,
    pub tempo: TempoData,
    /// The master bus. It is not part of `tracks` and can't be removed.
    pub master: Arc<Track>,
    /// Ordinary tracks and buses, in display order.
    pub tracks: Vec<Arc<Track>>,
}

impl Default for Project {
    fn default() -> Self {
        Self::new("Untitled")
    }
}

impl Project {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            sample_rate: 48_000,
            tempo: TempoData::default(),
            master: Arc::new(Track::new(TrackKind::Master, "Master")),
            tracks: Vec::new(),
        }
    }

    /// All tracks, master first.
    pub fn all_tracks(&self) -> impl Iterator<Item = &Arc<Track>> {
        std::iter::once(&self.master).chain(self.tracks.iter())
    }

    /// Every object ID in the project.
    pub fn all_ids(&self) -> impl Iterator<Item = uuid::Uuid> + '_ {
        self.all_tracks().flat_map(|t| t.ids())
    }

    pub fn track_index(&self, id: TrackId) -> Option<usize> {
        self.tracks.iter().position(|t| t.id == id)
    }

    /// Looks up any track, including the master.
    pub fn track(&self, id: TrackId) -> Option<&Arc<Track>> {
        self.all_tracks().find(|t| t.id == id)
    }

    /// Mutable access to any track, including the master. Copies the track if
    /// a snapshot still shares it.
    pub fn track_mut(&mut self, id: TrackId) -> Result<&mut Track, ModelError> {
        let arc = if self.master.id == id {
            &mut self.master
        } else {
            self.tracks
                .iter_mut()
                .find(|t| t.id == id)
                .ok_or(ModelError::TrackNotFound(id))?
        };
        Ok(Arc::make_mut(arc))
    }

    /// Replaces a whole track by ID. Used to restore saved versions on undo.
    pub(crate) fn replace_track(&mut self, track: Arc<Track>) -> Result<(), ModelError> {
        let id = track.id;
        let slot = if self.master.id == id {
            &mut self.master
        } else {
            self.tracks
                .iter_mut()
                .find(|t| t.id == id)
                .ok_or(ModelError::TrackNotFound(id))?
        };
        *slot = track;
        Ok(())
    }

    /// Finds the track holding a clip.
    pub fn clip_location(&self, id: ClipId) -> Option<(TrackId, usize)> {
        self.all_tracks()
            .find_map(|t| t.clips.iter().position(|c| c.id == id).map(|i| (t.id, i)))
    }

    pub fn clip(&self, id: ClipId) -> Option<&Arc<Clip>> {
        let (track, index) = self.clip_location(id)?;
        self.track(track).map(|t| &t.clips[index])
    }

    pub fn clip_mut(&mut self, id: ClipId) -> Result<&mut Clip, ModelError> {
        let (track, index) = self.clip_location(id).ok_or(ModelError::ClipNotFound(id))?;
        Ok(Arc::make_mut(&mut self.track_mut(track)?.clips[index]))
    }

    pub fn device_location(&self, id: DeviceId) -> Option<(TrackId, usize)> {
        self.all_tracks()
            .find_map(|t| t.devices.iter().position(|d| d.id == id).map(|i| (t.id, i)))
    }

    pub fn device_mut(&mut self, id: DeviceId) -> Result<&mut Device, ModelError> {
        let (track, index) = self
            .device_location(id)
            .ok_or(ModelError::DeviceNotFound(id))?;
        Ok(&mut self.track_mut(track)?.devices[index])
    }

    pub fn send_location(&self, id: SendId) -> Option<(TrackId, usize)> {
        self.all_tracks()
            .find_map(|t| t.sends.iter().position(|s| s.id == id).map(|i| (t.id, i)))
    }

    pub fn send_mut(&mut self, id: SendId) -> Result<&mut Send, ModelError> {
        let (track, index) = self.send_location(id).ok_or(ModelError::SendNotFound(id))?;
        Ok(&mut self.track_mut(track)?.sends[index])
    }

    pub fn lane_location(&self, id: LaneId) -> Option<(TrackId, usize)> {
        self.all_tracks().find_map(|t| {
            t.automation
                .iter()
                .position(|l| l.id == id)
                .map(|i| (t.id, i))
        })
    }

    pub fn lane_mut(&mut self, id: LaneId) -> Result<&mut AutomationLane, ModelError> {
        let (track, index) = self.lane_location(id).ok_or(ModelError::LaneNotFound(id))?;
        Ok(&mut self.track_mut(track)?.automation[index])
    }

    /// The routing graph: for each track, the tracks it sends audio to.
    fn routing_edges(&self) -> HashMap<TrackId, Vec<TrackId>> {
        self.all_tracks()
            .map(|t| {
                let output = match t.output {
                    TrackOutput::Master if t.kind == TrackKind::Master => None,
                    TrackOutput::Master => Some(self.master.id),
                    TrackOutput::Bus(b) => Some(b),
                };
                let targets = output.into_iter().chain(t.sends.iter().map(|s| s.target));
                (t.id, targets.collect())
            })
            .collect()
    }

    /// Whether audio could flow from `from` to `to` through outputs and sends.
    pub fn feeds(&self, from: TrackId, to: TrackId) -> bool {
        let edges = self.routing_edges();
        let mut seen = HashSet::new();
        let mut stack = vec![from];
        while let Some(t) = stack.pop() {
            if t == to {
                return true;
            }
            if seen.insert(t) {
                stack.extend(edges.get(&t).into_iter().flatten().copied());
            }
        }
        false
    }

    /// Whether `target` can receive audio from `source` (as its output or a
    /// send) without creating a loop.
    pub fn can_route(&self, source: TrackId, target: TrackId) -> Result<(), ModelError> {
        let Some(t) = self.track(target) else {
            return Err(ModelError::TrackNotFound(target));
        };
        if !matches!(t.kind, TrackKind::Bus | TrackKind::Master) {
            return Err(ModelError::Invalid(format!("track {target} is not a bus")));
        }
        if source == target || self.feeds(target, source) {
            return Err(ModelError::Invalid(format!(
                "routing {source} to {target} would create a loop"
            )));
        }
        Ok(())
    }

    /// Checks referential integrity: unique IDs, valid routing with no loops,
    /// clip types that match their tracks, and automation targets that exist.
    /// Every command keeps this true, and loading a file checks it.
    pub fn validate(&self) -> Result<(), ModelError> {
        let bad = |msg: String| Err(ModelError::Invalid(msg));
        if self.master.kind != TrackKind::Master {
            return bad("master track has the wrong kind".into());
        }
        self.tempo.validate()?;
        let mut ids = HashSet::new();
        if let Some(dup) = self.all_ids().find(|id| !ids.insert(*id)) {
            return bad(format!("duplicate id {dup}"));
        }
        for t in self.all_tracks() {
            if t.kind == TrackKind::Master && t.id != self.master.id {
                return bad(format!("track {} is a second master", t.id));
            }
            t.check()?;
        }
        for t in self.all_tracks() {
            if let TrackOutput::Bus(b) = t.output {
                if t.kind == TrackKind::Master {
                    return bad("master track can't route to a bus".into());
                }
                self.check_target(t.id, b)?;
            }
            for s in &t.sends {
                self.check_target(t.id, s.target)?;
            }
        }
        if has_cycle(&self.routing_edges()) {
            return bad("routing loop between tracks".into());
        }
        Ok(())
    }

    pub(crate) fn check_target(&self, source: TrackId, target: TrackId) -> Result<(), ModelError> {
        match self.track(target) {
            Some(t) if matches!(t.kind, TrackKind::Bus | TrackKind::Master) => Ok(()),
            Some(_) => Err(ModelError::Invalid(format!(
                "track {source} routes to {target}, which is not a bus"
            ))),
            None => Err(ModelError::TrackNotFound(target)),
        }
    }
}

fn has_cycle(edges: &HashMap<TrackId, Vec<TrackId>>) -> bool {
    // Iterative three-colour DFS: a grey node reached again closes a loop.
    #[derive(Copy, Clone, PartialEq)]
    enum Mark {
        Grey,
        Black,
    }
    let mut marks: HashMap<TrackId, Mark> = HashMap::new();
    for &root in edges.keys() {
        if marks.contains_key(&root) {
            continue;
        }
        let mut stack = vec![(root, 0usize)];
        marks.insert(root, Mark::Grey);
        while let Some((node, i)) = stack.pop() {
            let next = edges.get(&node).and_then(|e| e.get(i)).copied();
            match next {
                None => {
                    marks.insert(node, Mark::Black);
                }
                Some(n) => {
                    stack.push((node, i + 1));
                    match marks.get(&n) {
                        Some(Mark::Grey) => return true,
                        Some(Mark::Black) => {}
                        None => {
                            marks.insert(n, Mark::Grey);
                            stack.push((n, 0));
                        }
                    }
                }
            }
        }
    }
    false
}

pub(crate) fn invalid(msg: impl Into<String>) -> ModelError {
    ModelError::Invalid(msg.into())
}

pub(crate) fn check_finite(v: f64, what: &str) -> Result<(), ModelError> {
    if v.is_finite() {
        Ok(())
    } else {
        Err(invalid(format!("{what} must be finite")))
    }
}

pub(crate) fn check_points(points: &[AutomationPoint]) -> Result<(), ModelError> {
    if points.windows(2).any(|w| w[0].tick >= w[1].tick) {
        return Err(invalid(
            "automation points must be strictly increasing in time",
        ));
    }
    if points
        .iter()
        .any(|p| !p.value.is_finite() || !p.curve.is_finite())
    {
        return Err(invalid("automation values must be finite"));
    }
    Ok(())
}

/// Tempo and meter changes. The engine builds a `daw_core::time::TempoMap`
/// from this.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TempoData {
    /// Sorted by tick; the first point is at tick 0.
    pub tempos: Vec<TempoPoint>,
    /// Sorted by tick; the first one is at tick 0.
    pub meters: Vec<MeterPoint>,
}

impl Default for TempoData {
    fn default() -> Self {
        Self {
            tempos: vec![TempoPoint {
                tick: TickTime(0),
                bpm: 120.0,
                ramp: false,
            }],
            meters: vec![MeterPoint {
                tick: TickTime(0),
                numerator: 4,
                denominator: 4,
            }],
        }
    }
}

impl TempoData {
    pub fn validate(&self) -> Result<(), ModelError> {
        let bad = |m: &str| Err(ModelError::Invalid(m.into()));
        if self.tempos.first().map(|p| p.tick) != Some(TickTime(0)) {
            return bad("tempo map must start at tick 0");
        }
        if self.meters.first().map(|p| p.tick) != Some(TickTime(0)) {
            return bad("meter map must start at tick 0");
        }
        if self.tempos.windows(2).any(|w| w[0].tick >= w[1].tick)
            || self.meters.windows(2).any(|w| w[0].tick >= w[1].tick)
        {
            return bad("tempo and meter points must be strictly increasing");
        }
        if self
            .tempos
            .iter()
            .any(|p| !(p.bpm.is_finite() && p.bpm > 0.0))
        {
            return bad("tempo must be positive");
        }
        if self
            .meters
            .iter()
            .any(|m| m.numerator == 0 || !m.denominator.is_power_of_two())
        {
            return bad("bad time signature");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TempoPoint {
    pub tick: TickTime,
    pub bpm: f64,
    /// Ramp linearly to the next point instead of jumping.
    #[serde(default)]
    pub ramp: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MeterPoint {
    pub tick: TickTime,
    pub numerator: u8,
    pub denominator: u8,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackKind {
    Audio,
    Instrument,
    Bus,
    Master,
}

impl TrackKind {
    /// Whether clips with this content can live on this kind of track.
    pub fn accepts(self, content: &ClipContent) -> bool {
        matches!(
            (self, content),
            (TrackKind::Audio, ClipContent::Audio(_))
                | (TrackKind::Instrument, ClipContent::Midi(_))
        )
    }
}

/// Where a track's output goes.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackOutput {
    /// The master bus (for the master itself: the hardware output).
    Master,
    Bus(TrackId),
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Color(pub u8, pub u8, pub u8);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Track {
    pub id: TrackId,
    pub kind: TrackKind,
    pub name: String,
    #[serde(default)]
    pub color: Option<Color>,
    /// Fader gain in dB.
    pub volume_db: f64,
    /// -1 (left) to 1 (right).
    pub pan: f64,
    pub mute: bool,
    pub solo: bool,
    pub output: TrackOutput,
    /// Sorted by start tick.
    #[serde(default)]
    pub clips: Vec<Arc<Clip>>,
    /// Processing order: first device runs first.
    #[serde(default)]
    pub devices: Vec<Device>,
    #[serde(default)]
    pub sends: Vec<Send>,
    #[serde(default)]
    pub automation: Vec<AutomationLane>,
}

impl Track {
    pub fn new(kind: TrackKind, name: impl Into<String>) -> Self {
        Self {
            id: TrackId::new(),
            kind,
            name: name.into(),
            color: None,
            volume_db: 0.0,
            pan: 0.0,
            mute: false,
            solo: false,
            output: TrackOutput::Master,
            clips: Vec::new(),
            devices: Vec::new(),
            sends: Vec::new(),
            automation: Vec::new(),
        }
    }

    /// Checks everything about the track that doesn't depend on other
    /// tracks: value ranges, sorted clips that fit the track kind, and
    /// automation lanes that target this track's own sends and devices.
    pub fn check(&self) -> Result<(), ModelError> {
        let bad = |msg: String| Err(ModelError::Invalid(msg));
        check_finite(self.volume_db, "volume")?;
        if !(-1.0..=1.0).contains(&self.pan) {
            return bad(format!("track {} pan out of range", self.id));
        }
        if !self.clips.is_sorted_by_key(|c| c.start) {
            return bad(format!("clips on track {} are not sorted", self.id));
        }
        for c in &self.clips {
            if !self.kind.accepts(&c.content) {
                return bad(format!("clip {} doesn't fit track {}", c.id, self.id));
            }
            c.check()?;
        }
        for d in &self.devices {
            if d.params.values().any(|v| !v.is_finite()) {
                return bad(format!("device {} has a non-finite parameter", d.id));
            }
        }
        for s in &self.sends {
            check_finite(s.level_db, "send level")?;
        }
        let mut lane_targets = HashSet::new();
        for l in &self.automation {
            let ok = match l.target {
                AutomationTarget::Volume | AutomationTarget::Pan => true,
                AutomationTarget::SendLevel { send } => self.sends.iter().any(|x| x.id == send),
                AutomationTarget::DeviceParam { device, .. } => {
                    self.devices.iter().any(|x| x.id == device)
                }
            };
            if !ok {
                return bad(format!("lane {} targets something not on its track", l.id));
            }
            if !lane_targets.insert(l.target) {
                return bad(format!("track {} has two lanes for one target", self.id));
            }
            check_points(&l.points)?;
        }
        Ok(())
    }

    /// Every object ID in the track, including its clips' notes.
    pub fn ids(&self) -> impl Iterator<Item = uuid::Uuid> + '_ {
        let clips = self.clips.iter().flat_map(|c| {
            let notes = match &c.content {
                ClipContent::Midi(m) => &m.notes[..],
                ClipContent::Audio(_) => &[],
            };
            std::iter::once(c.id.0).chain(notes.iter().map(|n| n.id.0))
        });
        std::iter::once(self.id.0)
            .chain(clips)
            .chain(self.devices.iter().map(|d| d.id.0))
            .chain(self.sends.iter().map(|s| s.id.0))
            .chain(self.automation.iter().map(|l| l.id.0))
    }

    /// Where a clip starting at `start` belongs to keep `clips` sorted.
    pub(crate) fn clip_insert_index(&self, start: TickTime) -> usize {
        self.clips.partition_point(|c| c.start <= start)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Clip {
    pub id: ClipId,
    pub name: String,
    /// Position on the timeline.
    pub start: TickTime,
    pub length: TickTime,
    #[serde(default)]
    pub muted: bool,
    pub content: ClipContent,
}

impl Clip {
    /// Checks the clip's own values: positive length, sorted valid notes.
    pub fn check(&self) -> Result<(), ModelError> {
        if self.length.0 <= 0 || self.start.0 < 0 {
            return Err(invalid(
                "clip must start at or after 0 and have a positive length",
            ));
        }
        match &self.content {
            ClipContent::Audio(a) => {
                check_finite(a.gain_db, "clip gain")?;
                if a.offset.0 < 0 {
                    return Err(invalid("audio offset must not be negative"));
                }
            }
            ClipContent::Midi(m) => {
                for n in &m.notes {
                    n.check()?;
                }
                if !m.notes.is_sorted_by_key(MidiClip::sort_key) {
                    return Err(invalid("notes must be sorted"));
                }
            }
        }
        Ok(())
    }

    pub fn new(
        name: impl Into<String>,
        start: TickTime,
        length: TickTime,
        content: ClipContent,
    ) -> Self {
        Self {
            id: ClipId::new(),
            name: name.into(),
            start,
            length,
            muted: false,
            content,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClipContent {
    Audio(AudioClip),
    Midi(MidiClip),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AudioClip {
    /// Relative to the bundle root, for example `audio/take1.wav`. Absolute
    /// paths are allowed for files left outside the bundle.
    pub source: PathBuf,
    /// Where playback starts inside the file.
    pub offset: SampleTime,
    pub gain_db: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MidiClip {
    /// Sorted by start, then key. Note times are relative to the clip start.
    pub notes: Vec<Note>,
}

impl MidiClip {
    pub(crate) fn sort(&mut self) {
        self.notes.sort_by_key(Self::sort_key);
    }

    fn sort_key(n: &Note) -> (TickTime, u8, NoteUid) {
        (n.start, n.key, n.id)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Note {
    pub id: NoteUid,
    pub start: TickTime,
    pub length: TickTime,
    pub channel: u8,
    pub key: u8,
    /// 0 to 1.
    pub velocity: f64,
}

impl Note {
    /// Checks key, channel and velocity ranges and a positive length.
    pub fn check(&self) -> Result<(), ModelError> {
        if self.key > 127 || self.channel > 15 {
            return Err(invalid("note key or channel out of range"));
        }
        if self.length.0 <= 0 || self.start.0 < 0 {
            return Err(invalid(
                "note must start at or after 0 and have a positive length",
            ));
        }
        if !(0.0..=1.0).contains(&self.velocity) {
            return Err(invalid("velocity must be within 0..1"));
        }
        Ok(())
    }

    pub fn new(start: TickTime, length: TickTime, key: u8, velocity: f64) -> Self {
        Self {
            id: NoteUid::new(),
            start,
            length,
            channel: 0,
            key,
            velocity,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Device {
    pub id: DeviceId,
    pub name: String,
    pub enabled: bool,
    pub source: DeviceSource,
    /// Parameter values (plain, not normalized) that differ from the
    /// device's defaults.
    #[serde(default)]
    pub params: BTreeMap<ParamId, f64>,
    /// Opaque state from the plugin's own save function. Stored as base64.
    #[serde(default)]
    pub state: Option<StateBlob>,
}

impl Device {
    pub fn new(name: impl Into<String>, source: DeviceSource) -> Self {
        Self {
            id: DeviceId::new(),
            name: name.into(),
            enabled: true,
            source,
            params: BTreeMap::new(),
            state: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DeviceSource {
    /// One of our own devices, by catalog ID (see `daw-devices`).
    Builtin { kind: String },
    /// A plugin, by `PluginDescriptor::format` and `PluginDescriptor::id`.
    Plugin {
        format: PluginFormatKind,
        id: String,
    },
}

/// Opaque plugin state. Shared so copying a track doesn't copy it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct StateBlob(#[serde(with = "serde_util::base64_bytes")] pub Arc<[u8]>);

impl From<Vec<u8>> for StateBlob {
    fn from(v: Vec<u8>) -> Self {
        Self(v.into())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Send {
    pub id: SendId,
    /// A bus or the master.
    pub target: TrackId,
    pub level_db: f64,
    /// Tap the signal before the fader instead of after it.
    pub pre_fader: bool,
}

impl Send {
    pub fn new(target: TrackId) -> Self {
        Self {
            id: SendId::new(),
            target,
            level_db: 0.0,
            pre_fader: false,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AutomationTarget {
    Volume,
    Pan,
    SendLevel { send: SendId },
    DeviceParam { device: DeviceId, param: ParamId },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AutomationLane {
    pub id: LaneId,
    pub target: AutomationTarget,
    pub enabled: bool,
    /// Sorted by tick, at most one point per tick.
    pub points: Vec<AutomationPoint>,
}

impl AutomationLane {
    pub fn new(target: AutomationTarget) -> Self {
        Self {
            id: LaneId::new(),
            target,
            enabled: true,
            points: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AutomationPoint {
    pub tick: TickTime,
    /// In the target's own unit (dB, pan position, plain param value).
    pub value: f64,
    /// Shape of the segment to the next point: 0 is linear, negative bends
    /// toward the start value, positive toward the end value, and
    /// `f64::INFINITY` is not allowed. Use `step` for a jump.
    #[serde(default)]
    pub curve: f64,
    /// Hold this value until the next point, then jump.
    #[serde(default)]
    pub step: bool,
}
