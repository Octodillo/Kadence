//! The built-in edit commands.
//!
//! Most property edits ("Set" commands) work by swapping: `apply` swaps the
//! new value into the model and keeps the old one, and `revert` swaps it back.
//! That makes them their own inverse, and merging two consecutive edits of the
//! same property is just dropping the second one: the first still holds the
//! original value, and the model already holds the latest.
//!
//! Structural commands (add, remove, move) remember exact indices so that
//! undo restores the original order, not just the original contents.

use std::collections::{HashMap, HashSet};
use std::mem;
use std::sync::Arc;

use daw_core::event::ParamId;
use daw_core::time::{SampleTime, TickTime};
use uuid::Uuid;

use crate::change::{Change, ChangeSet};
use crate::command::Command;
use crate::error::ModelError;
use crate::id::{ClipId, DeviceId, LaneId, NoteUid, SendId, TrackId};
use crate::model::*;
use crate::model::{check_finite, check_points, invalid};

/// Fails if any of `ids` is already used anywhere in the project.
fn check_ids_free(project: &Project, ids: &[Uuid]) -> Result<(), ModelError> {
    let new: HashSet<Uuid> = ids.iter().copied().collect();
    if new.len() != ids.len() {
        return Err(invalid("new objects reuse an ID"));
    }
    if project.all_ids().any(|id| new.contains(&id)) {
        return Err(invalid("an ID is already in use"));
    }
    Ok(())
}

fn track_of(project: &Project, id: TrackId) -> Result<&Arc<Track>, ModelError> {
    project.track(id).ok_or(ModelError::TrackNotFound(id))
}

// ---------------------------------------------------------------------------
// Batches

/// Several commands that undo as one step, like "Paste" or "Delete
/// Selection". If one fails, the ones before it are reverted.
#[derive(Debug)]
pub struct Batch {
    pub label: String,
    pub commands: Vec<Box<dyn Command>>,
}

impl Batch {
    pub fn new(label: impl Into<String>, commands: Vec<Box<dyn Command>>) -> Self {
        Self {
            label: label.into(),
            commands,
        }
    }
}

impl Command for Batch {
    fn label(&self) -> String {
        self.label.clone()
    }

    fn apply(&mut self, project: &mut Project) -> Result<ChangeSet, ModelError> {
        let mut changes = ChangeSet::new();
        for i in 0..self.commands.len() {
            match self.commands[i].apply(project) {
                Ok(c) => changes.extend(c),
                Err(e) => {
                    for done in self.commands[..i].iter_mut().rev() {
                        let undone = done.revert(project);
                        debug_assert!(undone.is_ok(), "reverting a just-applied command failed");
                    }
                    return Err(e);
                }
            }
        }
        Ok(changes)
    }

    fn revert(&mut self, project: &mut Project) -> Result<ChangeSet, ModelError> {
        let mut changes = ChangeSet::new();
        for i in (0..self.commands.len()).rev() {
            match self.commands[i].revert(project) {
                Ok(c) => changes.extend(c),
                Err(e) => {
                    // Put back what this revert already undid, so a failed
                    // undo leaves the project as it was.
                    for undone in &mut self.commands[i + 1..] {
                        let redone = undone.apply(project);
                        debug_assert!(redone.is_ok(), "re-applying a just-reverted command failed");
                    }
                    return Err(e);
                }
            }
        }
        Ok(changes)
    }
}

// ---------------------------------------------------------------------------
// Project-wide

/// Renames the project.
#[derive(Debug)]
pub struct SetProjectName {
    pub name: String,
}

impl Command for SetProjectName {
    fn label(&self) -> String {
        "Rename Project".into()
    }

    fn apply(&mut self, project: &mut Project) -> Result<ChangeSet, ModelError> {
        mem::swap(&mut project.name, &mut self.name);
        Ok(ChangeSet::one(Change::Project))
    }

    fn revert(&mut self, project: &mut Project) -> Result<ChangeSet, ModelError> {
        self.apply(project)
    }

    fn merge(&mut self, next: &dyn Command) -> bool {
        next.downcast_ref::<Self>().is_some()
    }
}

/// Replaces the tempo and meter map.
#[derive(Debug)]
pub struct SetTempo {
    pub tempo: TempoData,
}

impl Command for SetTempo {
    fn label(&self) -> String {
        "Change Tempo".into()
    }

    fn apply(&mut self, project: &mut Project) -> Result<ChangeSet, ModelError> {
        self.tempo.validate()?;
        mem::swap(&mut project.tempo, &mut self.tempo);
        Ok(ChangeSet::one(Change::Tempo))
    }

    fn revert(&mut self, project: &mut Project) -> Result<ChangeSet, ModelError> {
        mem::swap(&mut project.tempo, &mut self.tempo);
        Ok(ChangeSet::one(Change::Tempo))
    }

    fn merge(&mut self, next: &dyn Command) -> bool {
        next.downcast_ref::<Self>().is_some()
    }
}

// ---------------------------------------------------------------------------
// Tracks

/// Inserts a track (possibly with clips, devices and so on, for duplicate or
/// paste) at `index` in the track list.
#[derive(Debug)]
pub struct AddTrack {
    pub index: usize,
    pub track: Arc<Track>,
}

impl AddTrack {
    pub fn new(index: usize, track: Track) -> Self {
        Self {
            index,
            track: Arc::new(track),
        }
    }
}

impl Command for AddTrack {
    fn label(&self) -> String {
        "Add Track".into()
    }

    fn apply(&mut self, project: &mut Project) -> Result<ChangeSet, ModelError> {
        if self.track.kind == TrackKind::Master {
            return Err(invalid("can't add a second master track"));
        }
        if self.index > project.tracks.len() {
            return Err(invalid("track index out of range"));
        }
        let t = &self.track;
        t.check()?;
        let ids: Vec<Uuid> = t.ids().collect();
        check_ids_free(project, &ids)?;
        // Nothing routes into a new track yet, so it can't close a loop
        // unless it routes to itself; its targets just have to be buses.
        let targets = match t.output {
            TrackOutput::Bus(b) => Some(b),
            TrackOutput::Master => None,
        };
        for target in targets.into_iter().chain(t.sends.iter().map(|s| s.target)) {
            if target == t.id {
                return Err(invalid("a track can't route to itself"));
            }
            project.check_target(t.id, target)?;
        }
        project.tracks.insert(self.index, self.track.clone());
        Ok(ChangeSet::one(Change::TrackList))
    }

    fn revert(&mut self, project: &mut Project) -> Result<ChangeSet, ModelError> {
        let i = project
            .track_index(self.track.id)
            .ok_or(ModelError::TrackNotFound(self.track.id))?;
        self.track = project.tracks.remove(i);
        Ok(ChangeSet::one(Change::TrackList))
    }
}

#[derive(Debug)]
struct RemovedTrack {
    index: usize,
    track: Arc<Track>,
    /// Other tracks as they were before their routing to this one was cut.
    touched: Vec<Arc<Track>>,
}

/// Removes a track. Tracks that output to it are rerouted to the master, and
/// sends to it (with their automation) are removed.
#[derive(Debug)]
pub struct RemoveTrack {
    pub id: TrackId,
    saved: Option<RemovedTrack>,
}

impl RemoveTrack {
    pub fn new(id: TrackId) -> Self {
        Self { id, saved: None }
    }
}

impl Command for RemoveTrack {
    fn label(&self) -> String {
        "Remove Track".into()
    }

    fn apply(&mut self, project: &mut Project) -> Result<ChangeSet, ModelError> {
        let id = self.id;
        let index = project.track_index(id).ok_or_else(|| {
            if project.master.id == id {
                invalid("the master track can't be removed")
            } else {
                ModelError::TrackNotFound(id)
            }
        })?;
        let mut changes = ChangeSet::one(Change::TrackList);
        let mut touched = Vec::new();
        for arc in project
            .tracks
            .iter_mut()
            .chain(std::iter::once(&mut project.master))
        {
            let routes_here =
                arc.output == TrackOutput::Bus(id) || arc.sends.iter().any(|s| s.target == id);
            if arc.id == id || !routes_here {
                continue;
            }
            touched.push(arc.clone());
            let t = Arc::make_mut(arc);
            if t.output == TrackOutput::Bus(id) {
                t.output = TrackOutput::Master;
                changes.push(Change::Track(t.id));
            }
            let dropped: Vec<SendId> = t
                .sends
                .iter()
                .filter(|s| s.target == id)
                .map(|s| s.id)
                .collect();
            if !dropped.is_empty() {
                t.sends.retain(|s| s.target != id);
                changes.push(Change::SendList(t.id));
                let before = t.automation.len();
                t.automation.retain(|l| !matches!(l.target, AutomationTarget::SendLevel { send } if dropped.contains(&send)));
                if t.automation.len() != before {
                    changes.push(Change::LaneList(t.id));
                }
            }
        }
        let track = project.tracks.remove(index);
        self.saved = Some(RemovedTrack {
            index,
            track,
            touched,
        });
        Ok(changes)
    }

    fn revert(&mut self, project: &mut Project) -> Result<ChangeSet, ModelError> {
        let saved = self
            .saved
            .take()
            .ok_or_else(|| invalid("RemoveTrack reverted before apply"))?;
        let mut changes = ChangeSet::one(Change::TrackList);
        project.tracks.insert(saved.index, saved.track);
        for t in saved.touched {
            changes.push(Change::Track(t.id));
            changes.push(Change::SendList(t.id));
            changes.push(Change::LaneList(t.id));
            project.replace_track(t)?;
        }
        Ok(changes)
    }
}

/// Moves a track to a new position in the track list. `to` is the index the
/// track ends up at.
#[derive(Debug)]
pub struct MoveTrack {
    pub id: TrackId,
    pub to: usize,
    from: usize,
}

impl MoveTrack {
    pub fn new(id: TrackId, to: usize) -> Self {
        Self { id, to, from: 0 }
    }
}

impl Command for MoveTrack {
    fn label(&self) -> String {
        "Move Track".into()
    }

    fn apply(&mut self, project: &mut Project) -> Result<ChangeSet, ModelError> {
        let from = project
            .track_index(self.id)
            .ok_or(ModelError::TrackNotFound(self.id))?;
        if self.to >= project.tracks.len() {
            return Err(invalid("track index out of range"));
        }
        let t = project.tracks.remove(from);
        project.tracks.insert(self.to, t);
        self.from = from;
        Ok(ChangeSet::one(Change::TrackList))
    }

    fn revert(&mut self, project: &mut Project) -> Result<ChangeSet, ModelError> {
        let t = project.tracks.remove(self.to);
        project.tracks.insert(self.from, t);
        Ok(ChangeSet::one(Change::TrackList))
    }
}

/// A track property and its value.
#[derive(Clone, Debug, PartialEq)]
pub enum TrackField {
    Name(String),
    Color(Option<Color>),
    VolumeDb(f64),
    Pan(f64),
    Mute(bool),
    Solo(bool),
    Output(TrackOutput),
}

/// Sets one property of a track. Consecutive edits of the same property on the
/// same track merge into one undo step.
#[derive(Debug)]
pub struct SetTrack {
    pub id: TrackId,
    pub field: TrackField,
}

impl SetTrack {
    pub fn new(id: TrackId, field: TrackField) -> Self {
        Self { id, field }
    }

    fn swap(&mut self, project: &mut Project) -> Result<ChangeSet, ModelError> {
        let t = project.track_mut(self.id)?;
        match &mut self.field {
            TrackField::Name(v) => mem::swap(&mut t.name, v),
            TrackField::Color(v) => mem::swap(&mut t.color, v),
            TrackField::VolumeDb(v) => mem::swap(&mut t.volume_db, v),
            TrackField::Pan(v) => mem::swap(&mut t.pan, v),
            TrackField::Mute(v) => mem::swap(&mut t.mute, v),
            TrackField::Solo(v) => mem::swap(&mut t.solo, v),
            TrackField::Output(v) => mem::swap(&mut t.output, v),
        }
        Ok(ChangeSet::one(Change::Track(self.id)))
    }
}

impl Command for SetTrack {
    fn label(&self) -> String {
        match self.field {
            TrackField::Name(_) => "Rename Track",
            TrackField::Color(_) => "Change Track Color",
            TrackField::VolumeDb(_) => "Change Volume",
            TrackField::Pan(_) => "Change Pan",
            TrackField::Mute(_) => "Mute Track",
            TrackField::Solo(_) => "Solo Track",
            TrackField::Output(_) => "Change Track Output",
        }
        .into()
    }

    fn apply(&mut self, project: &mut Project) -> Result<ChangeSet, ModelError> {
        let track = track_of(project, self.id)?;
        match self.field {
            TrackField::VolumeDb(v) => check_finite(v, "volume")?,
            TrackField::Pan(v) if !(-1.0..=1.0).contains(&v) => {
                return Err(invalid("pan must be within -1..1"));
            }
            TrackField::Output(TrackOutput::Bus(b)) => {
                if track.kind == TrackKind::Master {
                    return Err(invalid("the master track can't route to a bus"));
                }
                project.can_route(self.id, b)?;
            }
            _ => {}
        }
        self.swap(project)
    }

    fn revert(&mut self, project: &mut Project) -> Result<ChangeSet, ModelError> {
        self.swap(project)
    }

    fn merge(&mut self, next: &dyn Command) -> bool {
        next.downcast_ref::<Self>().is_some_and(|n| {
            n.id == self.id && mem::discriminant(&n.field) == mem::discriminant(&self.field)
        })
    }
}

// ---------------------------------------------------------------------------
// Clips

/// Adds a clip to a track, keeping the track's clips sorted by start.
#[derive(Debug)]
pub struct AddClip {
    pub track: TrackId,
    pub clip: Arc<Clip>,
}

impl AddClip {
    pub fn new(track: TrackId, clip: Clip) -> Self {
        Self {
            track,
            clip: Arc::new(clip),
        }
    }
}

impl Command for AddClip {
    fn label(&self) -> String {
        "Add Clip".into()
    }

    fn apply(&mut self, project: &mut Project) -> Result<ChangeSet, ModelError> {
        let track = track_of(project, self.track)?;
        if !track.kind.accepts(&self.clip.content) {
            return Err(invalid("this kind of clip doesn't fit on this track"));
        }
        self.clip.check()?;
        let mut ids = vec![self.clip.id.0];
        if let ClipContent::Midi(m) = &self.clip.content {
            ids.extend(m.notes.iter().map(|n| n.id.0));
        }
        check_ids_free(project, &ids)?;
        let t = project.track_mut(self.track)?;
        let i = t.clip_insert_index(self.clip.start);
        t.clips.insert(i, self.clip.clone());
        Ok(ChangeSet::one(Change::ClipList(self.track)))
    }

    fn revert(&mut self, project: &mut Project) -> Result<ChangeSet, ModelError> {
        let t = project.track_mut(self.track)?;
        let i = t
            .clips
            .iter()
            .position(|c| c.id == self.clip.id)
            .ok_or(ModelError::ClipNotFound(self.clip.id))?;
        self.clip = t.clips.remove(i);
        Ok(ChangeSet::one(Change::ClipList(self.track)))
    }
}

/// Removes a clip.
#[derive(Debug)]
pub struct RemoveClip {
    pub id: ClipId,
    saved: Option<(TrackId, usize, Arc<Clip>)>,
}

impl RemoveClip {
    pub fn new(id: ClipId) -> Self {
        Self { id, saved: None }
    }
}

impl Command for RemoveClip {
    fn label(&self) -> String {
        "Remove Clip".into()
    }

    fn apply(&mut self, project: &mut Project) -> Result<ChangeSet, ModelError> {
        let (track, i) = project
            .clip_location(self.id)
            .ok_or(ModelError::ClipNotFound(self.id))?;
        let clip = project.track_mut(track)?.clips.remove(i);
        self.saved = Some((track, i, clip));
        Ok(ChangeSet::one(Change::ClipList(track)))
    }

    fn revert(&mut self, project: &mut Project) -> Result<ChangeSet, ModelError> {
        let (track, i, clip) = self
            .saved
            .take()
            .ok_or_else(|| invalid("RemoveClip reverted before apply"))?;
        project.track_mut(track)?.clips.insert(i, clip);
        Ok(ChangeSet::one(Change::ClipList(track)))
    }
}

/// Moves a clip in time and optionally to another track of the same kind.
/// Consecutive moves of the same clip (a drag) merge.
#[derive(Debug)]
pub struct MoveClip {
    pub id: ClipId,
    pub track: TrackId,
    pub start: TickTime,
    /// Where the clip was: track, index in that track, start.
    old: Option<(TrackId, usize, TickTime)>,
}

impl MoveClip {
    pub fn new(id: ClipId, track: TrackId, start: TickTime) -> Self {
        Self {
            id,
            track,
            start,
            old: None,
        }
    }
}

impl Command for MoveClip {
    fn label(&self) -> String {
        "Move Clip".into()
    }

    fn apply(&mut self, project: &mut Project) -> Result<ChangeSet, ModelError> {
        let (from, i) = project
            .clip_location(self.id)
            .ok_or(ModelError::ClipNotFound(self.id))?;
        let dest = track_of(project, self.track)?;
        if self.start.0 < 0 {
            return Err(invalid("clips can't start before 0"));
        }
        if !dest
            .kind
            .accepts(&project.track(from).expect("found above").clips[i].content)
        {
            return Err(invalid("this kind of clip doesn't fit on that track"));
        }
        let mut arc = project.track_mut(from)?.clips.remove(i);
        let old_start = mem::replace(&mut Arc::make_mut(&mut arc).start, self.start);
        let t = project.track_mut(self.track)?;
        let j = t.clip_insert_index(self.start);
        t.clips.insert(j, arc);
        self.old = Some((from, i, old_start));
        let mut changes: ChangeSet = [
            Change::ClipList(from),
            Change::ClipList(self.track),
            Change::Clip(self.id),
        ]
        .into_iter()
        .collect();
        if from == self.track {
            changes = [Change::ClipList(from), Change::Clip(self.id)]
                .into_iter()
                .collect();
        }
        Ok(changes)
    }

    fn revert(&mut self, project: &mut Project) -> Result<ChangeSet, ModelError> {
        let (from, i, start) = self
            .old
            .take()
            .ok_or_else(|| invalid("MoveClip reverted before apply"))?;
        let (now, j) = project
            .clip_location(self.id)
            .ok_or(ModelError::ClipNotFound(self.id))?;
        let mut arc = project.track_mut(now)?.clips.remove(j);
        Arc::make_mut(&mut arc).start = start;
        project.track_mut(from)?.clips.insert(i, arc);
        Ok([
            Change::ClipList(from),
            Change::ClipList(now),
            Change::Clip(self.id),
        ]
        .into_iter()
        .collect())
    }

    fn merge(&mut self, next: &dyn Command) -> bool {
        match next.downcast_ref::<Self>() {
            Some(n) if n.id == self.id => {
                self.track = n.track;
                self.start = n.start;
                true
            }
            _ => false,
        }
    }
}

/// A clip property other than its position.
#[derive(Clone, Debug, PartialEq)]
pub enum ClipField {
    Name(String),
    Length(TickTime),
    Muted(bool),
    /// Audio clips only.
    AudioOffset(SampleTime),
    /// Audio clips only.
    AudioGainDb(f64),
}

/// Sets one property of a clip. Consecutive edits of the same property on the
/// same clip merge.
#[derive(Debug)]
pub struct SetClip {
    pub id: ClipId,
    pub field: ClipField,
}

impl SetClip {
    pub fn new(id: ClipId, field: ClipField) -> Self {
        Self { id, field }
    }

    fn swap(&mut self, project: &mut Project) -> Result<ChangeSet, ModelError> {
        let c = project.clip_mut(self.id)?;
        match (&mut self.field, &mut c.content) {
            (ClipField::Name(v), _) => mem::swap(&mut c.name, v),
            (ClipField::Length(v), _) => mem::swap(&mut c.length, v),
            (ClipField::Muted(v), _) => mem::swap(&mut c.muted, v),
            (ClipField::AudioOffset(v), ClipContent::Audio(a)) => mem::swap(&mut a.offset, v),
            (ClipField::AudioGainDb(v), ClipContent::Audio(a)) => mem::swap(&mut a.gain_db, v),
            _ => return Err(invalid("that property only applies to audio clips")),
        }
        Ok(ChangeSet::one(Change::Clip(self.id)))
    }
}

impl Command for SetClip {
    fn label(&self) -> String {
        match self.field {
            ClipField::Name(_) => "Rename Clip",
            ClipField::Length(_) => "Resize Clip",
            ClipField::Muted(_) => "Mute Clip",
            ClipField::AudioOffset(_) => "Slip Clip",
            ClipField::AudioGainDb(_) => "Change Clip Gain",
        }
        .into()
    }

    fn apply(&mut self, project: &mut Project) -> Result<ChangeSet, ModelError> {
        let clip = project
            .clip(self.id)
            .ok_or(ModelError::ClipNotFound(self.id))?;
        match self.field {
            ClipField::Length(l) if l.0 <= 0 => {
                return Err(invalid("clip length must be positive"));
            }
            ClipField::AudioOffset(o) if o.0 < 0 => {
                return Err(invalid("audio offset must not be negative"));
            }
            ClipField::AudioGainDb(g) => check_finite(g, "clip gain")?,
            _ => {}
        }
        let audio_only = matches!(
            self.field,
            ClipField::AudioOffset(_) | ClipField::AudioGainDb(_)
        );
        if audio_only && !matches!(clip.content, ClipContent::Audio(_)) {
            return Err(invalid("that property only applies to audio clips"));
        }
        self.swap(project)
    }

    fn revert(&mut self, project: &mut Project) -> Result<ChangeSet, ModelError> {
        self.swap(project)
    }

    fn merge(&mut self, next: &dyn Command) -> bool {
        next.downcast_ref::<Self>().is_some_and(|n| {
            n.id == self.id && mem::discriminant(&n.field) == mem::discriminant(&self.field)
        })
    }
}

// ---------------------------------------------------------------------------
// Notes

fn midi_mut(project: &mut Project, clip: ClipId) -> Result<&mut MidiClip, ModelError> {
    match &mut project.clip_mut(clip)?.content {
        ClipContent::Midi(m) => Ok(m),
        ClipContent::Audio(_) => Err(invalid("not a MIDI clip")),
    }
}

fn midi(project: &Project, clip: ClipId) -> Result<&MidiClip, ModelError> {
    match &project
        .clip(clip)
        .ok_or(ModelError::ClipNotFound(clip))?
        .content
    {
        ClipContent::Midi(m) => Ok(m),
        ClipContent::Audio(_) => Err(invalid("not a MIDI clip")),
    }
}

/// Adds notes to a MIDI clip.
#[derive(Debug)]
pub struct AddNotes {
    pub clip: ClipId,
    pub notes: Vec<Note>,
}

impl Command for AddNotes {
    fn label(&self) -> String {
        if self.notes.len() == 1 {
            "Add Note"
        } else {
            "Add Notes"
        }
        .into()
    }

    fn apply(&mut self, project: &mut Project) -> Result<ChangeSet, ModelError> {
        midi(project, self.clip)?;
        for n in &self.notes {
            n.check()?;
        }
        let ids: Vec<Uuid> = self.notes.iter().map(|n| n.id.0).collect();
        check_ids_free(project, &ids)?;
        let m = midi_mut(project, self.clip)?;
        m.notes.extend(self.notes.iter().cloned());
        m.sort();
        Ok(ChangeSet::one(Change::Clip(self.clip)))
    }

    fn revert(&mut self, project: &mut Project) -> Result<ChangeSet, ModelError> {
        let ids: HashSet<NoteUid> = self.notes.iter().map(|n| n.id).collect();
        midi_mut(project, self.clip)?
            .notes
            .retain(|n| !ids.contains(&n.id));
        Ok(ChangeSet::one(Change::Clip(self.clip)))
    }
}

/// Removes notes from a MIDI clip.
#[derive(Debug)]
pub struct RemoveNotes {
    pub clip: ClipId,
    pub ids: Vec<NoteUid>,
    removed: Vec<Note>,
}

impl RemoveNotes {
    pub fn new(clip: ClipId, ids: Vec<NoteUid>) -> Self {
        Self {
            clip,
            ids,
            removed: Vec::new(),
        }
    }
}

impl Command for RemoveNotes {
    fn label(&self) -> String {
        if self.ids.len() == 1 {
            "Delete Note"
        } else {
            "Delete Notes"
        }
        .into()
    }

    fn apply(&mut self, project: &mut Project) -> Result<ChangeSet, ModelError> {
        let ids: HashSet<NoteUid> = self.ids.iter().copied().collect();
        let m = midi(project, self.clip)?;
        if let Some(missing) = ids.iter().find(|id| !m.notes.iter().any(|n| n.id == **id)) {
            return Err(ModelError::NoteNotFound(*missing));
        }
        let m = midi_mut(project, self.clip)?;
        let (removed, kept) = mem::take(&mut m.notes)
            .into_iter()
            .partition(|n| ids.contains(&n.id));
        m.notes = kept;
        self.removed = removed;
        Ok(ChangeSet::one(Change::Clip(self.clip)))
    }

    fn revert(&mut self, project: &mut Project) -> Result<ChangeSet, ModelError> {
        let m = midi_mut(project, self.clip)?;
        m.notes.append(&mut self.removed);
        m.sort();
        Ok(ChangeSet::one(Change::Clip(self.clip)))
    }
}

/// Replaces notes (matched by ID) with edited versions: moves, resizes,
/// velocity and pitch changes. Consecutive edits of the same set of notes
/// merge.
#[derive(Debug)]
pub struct EditNotes {
    pub clip: ClipId,
    pub notes: Vec<Note>,
}

impl EditNotes {
    fn swap(&mut self, project: &mut Project) -> Result<ChangeSet, ModelError> {
        let m = midi_mut(project, self.clip)?;
        let index: HashMap<NoteUid, usize> =
            m.notes.iter().enumerate().map(|(i, n)| (n.id, i)).collect();
        for new in &self.notes {
            if !index.contains_key(&new.id) {
                return Err(ModelError::NoteNotFound(new.id));
            }
        }
        for new in &mut self.notes {
            mem::swap(&mut m.notes[index[&new.id]], new);
        }
        m.sort();
        Ok(ChangeSet::one(Change::Clip(self.clip)))
    }
}

impl Command for EditNotes {
    fn label(&self) -> String {
        if self.notes.len() == 1 {
            "Edit Note"
        } else {
            "Edit Notes"
        }
        .into()
    }

    fn apply(&mut self, project: &mut Project) -> Result<ChangeSet, ModelError> {
        midi(project, self.clip)?;
        let mut seen = HashSet::new();
        for n in &self.notes {
            n.check()?;
            if !seen.insert(n.id) {
                return Err(invalid("a note is listed twice"));
            }
        }
        // `swap` checks that every note exists before changing anything.
        self.swap(project)
    }

    fn revert(&mut self, project: &mut Project) -> Result<ChangeSet, ModelError> {
        self.swap(project)
    }

    fn merge(&mut self, next: &dyn Command) -> bool {
        next.downcast_ref::<Self>().is_some_and(|n| {
            if n.clip != self.clip || n.notes.len() != self.notes.len() {
                return false;
            }
            let mine: HashSet<NoteUid> = self.notes.iter().map(|x| x.id).collect();
            n.notes.iter().all(|x| mine.contains(&x.id))
        })
    }
}

// ---------------------------------------------------------------------------
// Devices

/// Inserts a device into a track's chain at `index`.
#[derive(Debug)]
pub struct AddDevice {
    pub track: TrackId,
    pub index: usize,
    pub device: Device,
}

impl Command for AddDevice {
    fn label(&self) -> String {
        "Add Device".into()
    }

    fn apply(&mut self, project: &mut Project) -> Result<ChangeSet, ModelError> {
        if self.index > track_of(project, self.track)?.devices.len() {
            return Err(invalid("device index out of range"));
        }
        if self.device.params.values().any(|v| !v.is_finite()) {
            return Err(invalid("parameter values must be finite"));
        }
        check_ids_free(project, &[self.device.id.0])?;
        project
            .track_mut(self.track)?
            .devices
            .insert(self.index, self.device.clone());
        Ok(ChangeSet::one(Change::DeviceList(self.track)))
    }

    fn revert(&mut self, project: &mut Project) -> Result<ChangeSet, ModelError> {
        self.device = project.track_mut(self.track)?.devices.remove(self.index);
        Ok(ChangeSet::one(Change::DeviceList(self.track)))
    }
}

/// Removes a device and any automation lanes for its parameters.
#[derive(Debug)]
pub struct RemoveDevice {
    pub id: DeviceId,
    saved: Option<(TrackId, usize, Device, TakenLanes)>,
}

impl RemoveDevice {
    pub fn new(id: DeviceId) -> Self {
        Self { id, saved: None }
    }
}

/// Lanes removed along with their target, with their former indices.
type TakenLanes = Vec<(usize, AutomationLane)>;

/// Removes the lanes matching `pred`, returning them with their indices.
fn take_lanes(t: &mut Track, pred: impl Fn(&AutomationTarget) -> bool) -> TakenLanes {
    let mut taken = Vec::new();
    let mut kept = Vec::with_capacity(t.automation.len());
    for (i, lane) in mem::take(&mut t.automation).into_iter().enumerate() {
        if pred(&lane.target) {
            taken.push((i, lane));
        } else {
            kept.push(lane);
        }
    }
    t.automation = kept;
    taken
}

fn restore_lanes(t: &mut Track, lanes: TakenLanes) {
    for (i, lane) in lanes {
        t.automation.insert(i, lane);
    }
}

impl Command for RemoveDevice {
    fn label(&self) -> String {
        "Remove Device".into()
    }

    fn apply(&mut self, project: &mut Project) -> Result<ChangeSet, ModelError> {
        let (track, i) = project
            .device_location(self.id)
            .ok_or(ModelError::DeviceNotFound(self.id))?;
        let t = project.track_mut(track)?;
        let device = t.devices.remove(i);
        let id = self.id;
        let lanes = take_lanes(
            t,
            |target| matches!(target, AutomationTarget::DeviceParam { device, .. } if *device == id),
        );
        let mut changes = ChangeSet::one(Change::DeviceList(track));
        if !lanes.is_empty() {
            changes.push(Change::LaneList(track));
        }
        self.saved = Some((track, i, device, lanes));
        Ok(changes)
    }

    fn revert(&mut self, project: &mut Project) -> Result<ChangeSet, ModelError> {
        let (track, i, device, lanes) = self
            .saved
            .take()
            .ok_or_else(|| invalid("RemoveDevice reverted before apply"))?;
        let t = project.track_mut(track)?;
        t.devices.insert(i, device);
        let mut changes = ChangeSet::one(Change::DeviceList(track));
        if !lanes.is_empty() {
            changes.push(Change::LaneList(track));
        }
        restore_lanes(t, lanes);
        Ok(changes)
    }
}

/// Moves a device within its track's chain. `to` is its final index.
#[derive(Debug)]
pub struct MoveDevice {
    pub id: DeviceId,
    pub to: usize,
    from: Option<(TrackId, usize)>,
}

impl MoveDevice {
    pub fn new(id: DeviceId, to: usize) -> Self {
        Self { id, to, from: None }
    }
}

impl Command for MoveDevice {
    fn label(&self) -> String {
        "Move Device".into()
    }

    fn apply(&mut self, project: &mut Project) -> Result<ChangeSet, ModelError> {
        let (track, i) = project
            .device_location(self.id)
            .ok_or(ModelError::DeviceNotFound(self.id))?;
        let t = project.track_mut(track)?;
        if self.to >= t.devices.len() {
            return Err(invalid("device index out of range"));
        }
        let d = t.devices.remove(i);
        t.devices.insert(self.to, d);
        self.from = Some((track, i));
        Ok(ChangeSet::one(Change::DeviceList(track)))
    }

    fn revert(&mut self, project: &mut Project) -> Result<ChangeSet, ModelError> {
        let (track, i) = self
            .from
            .take()
            .ok_or_else(|| invalid("MoveDevice reverted before apply"))?;
        let t = project.track_mut(track)?;
        let d = t.devices.remove(self.to);
        t.devices.insert(i, d);
        Ok(ChangeSet::one(Change::DeviceList(track)))
    }
}

/// A device property other than its parameters.
#[derive(Clone, Debug, PartialEq)]
pub enum DeviceField {
    Name(String),
    Enabled(bool),
    /// The plugin's saved state, for example after closing its editor.
    State(Option<StateBlob>),
}

/// Sets one property of a device.
#[derive(Debug)]
pub struct SetDevice {
    pub id: DeviceId,
    pub field: DeviceField,
}

impl SetDevice {
    fn swap(&mut self, project: &mut Project) -> Result<ChangeSet, ModelError> {
        let d = project.device_mut(self.id)?;
        match &mut self.field {
            DeviceField::Name(v) => mem::swap(&mut d.name, v),
            DeviceField::Enabled(v) => mem::swap(&mut d.enabled, v),
            DeviceField::State(v) => mem::swap(&mut d.state, v),
        }
        Ok(ChangeSet::one(Change::Device(self.id)))
    }
}

impl Command for SetDevice {
    fn label(&self) -> String {
        match self.field {
            DeviceField::Name(_) => "Rename Device",
            DeviceField::Enabled(_) => "Bypass Device",
            DeviceField::State(_) => "Change Plugin State",
        }
        .into()
    }

    fn apply(&mut self, project: &mut Project) -> Result<ChangeSet, ModelError> {
        self.swap(project)
    }

    fn revert(&mut self, project: &mut Project) -> Result<ChangeSet, ModelError> {
        self.swap(project)
    }

    fn merge(&mut self, next: &dyn Command) -> bool {
        next.downcast_ref::<Self>().is_some_and(|n| {
            n.id == self.id && mem::discriminant(&n.field) == mem::discriminant(&self.field)
        })
    }
}

/// Sets a device parameter. `None` returns it to the device's default.
/// Consecutive edits of the same parameter (a knob drag) merge.
#[derive(Debug)]
pub struct SetParam {
    pub device: DeviceId,
    pub param: ParamId,
    pub value: Option<f64>,
}

impl SetParam {
    fn swap(&mut self, project: &mut Project) -> Result<ChangeSet, ModelError> {
        let d = project.device_mut(self.device)?;
        self.value = match self.value.take() {
            Some(v) => d.params.insert(self.param, v),
            None => d.params.remove(&self.param),
        };
        Ok(ChangeSet::one(Change::Device(self.device)))
    }
}

impl Command for SetParam {
    fn label(&self) -> String {
        "Change Parameter".into()
    }

    fn apply(&mut self, project: &mut Project) -> Result<ChangeSet, ModelError> {
        if let Some(v) = self.value {
            check_finite(v, "parameter value")?;
        }
        self.swap(project)
    }

    fn revert(&mut self, project: &mut Project) -> Result<ChangeSet, ModelError> {
        self.swap(project)
    }

    fn merge(&mut self, next: &dyn Command) -> bool {
        next.downcast_ref::<Self>()
            .is_some_and(|n| n.device == self.device && n.param == self.param)
    }
}

// ---------------------------------------------------------------------------
// Sends

/// Adds a send from a track to a bus.
#[derive(Debug)]
pub struct AddSend {
    pub track: TrackId,
    pub send: Send,
}

impl Command for AddSend {
    fn label(&self) -> String {
        "Add Send".into()
    }

    fn apply(&mut self, project: &mut Project) -> Result<ChangeSet, ModelError> {
        track_of(project, self.track)?;
        check_finite(self.send.level_db, "send level")?;
        check_ids_free(project, &[self.send.id.0])?;
        project.can_route(self.track, self.send.target)?;
        project.track_mut(self.track)?.sends.push(self.send.clone());
        Ok(ChangeSet::one(Change::SendList(self.track)))
    }

    fn revert(&mut self, project: &mut Project) -> Result<ChangeSet, ModelError> {
        let t = project.track_mut(self.track)?;
        self.send = t
            .sends
            .pop()
            .ok_or(ModelError::SendNotFound(self.send.id))?;
        Ok(ChangeSet::one(Change::SendList(self.track)))
    }
}

/// Removes a send and its automation.
#[derive(Debug)]
pub struct RemoveSend {
    pub id: SendId,
    saved: Option<(TrackId, usize, Send, TakenLanes)>,
}

impl RemoveSend {
    pub fn new(id: SendId) -> Self {
        Self { id, saved: None }
    }
}

impl Command for RemoveSend {
    fn label(&self) -> String {
        "Remove Send".into()
    }

    fn apply(&mut self, project: &mut Project) -> Result<ChangeSet, ModelError> {
        let (track, i) = project
            .send_location(self.id)
            .ok_or(ModelError::SendNotFound(self.id))?;
        let t = project.track_mut(track)?;
        let send = t.sends.remove(i);
        let id = self.id;
        let lanes = take_lanes(t, |target| {
            *target == AutomationTarget::SendLevel { send: id }
        });
        let mut changes = ChangeSet::one(Change::SendList(track));
        if !lanes.is_empty() {
            changes.push(Change::LaneList(track));
        }
        self.saved = Some((track, i, send, lanes));
        Ok(changes)
    }

    fn revert(&mut self, project: &mut Project) -> Result<ChangeSet, ModelError> {
        let (track, i, send, lanes) = self
            .saved
            .take()
            .ok_or_else(|| invalid("RemoveSend reverted before apply"))?;
        let t = project.track_mut(track)?;
        t.sends.insert(i, send);
        let mut changes = ChangeSet::one(Change::SendList(track));
        if !lanes.is_empty() {
            changes.push(Change::LaneList(track));
        }
        restore_lanes(t, lanes);
        Ok(changes)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum SendField {
    LevelDb(f64),
    PreFader(bool),
    Target(TrackId),
}

/// Sets one property of a send.
#[derive(Debug)]
pub struct SetSend {
    pub id: SendId,
    pub field: SendField,
}

impl SetSend {
    fn swap(&mut self, project: &mut Project) -> Result<ChangeSet, ModelError> {
        let s = project.send_mut(self.id)?;
        match &mut self.field {
            SendField::LevelDb(v) => mem::swap(&mut s.level_db, v),
            SendField::PreFader(v) => mem::swap(&mut s.pre_fader, v),
            SendField::Target(v) => mem::swap(&mut s.target, v),
        }
        let mut changes = ChangeSet::one(Change::Send(self.id));
        if let SendField::Target(_) = self.field {
            let (track, _) = project.send_location(self.id).expect("found above");
            changes.push(Change::SendList(track));
        }
        Ok(changes)
    }
}

impl Command for SetSend {
    fn label(&self) -> String {
        match self.field {
            SendField::LevelDb(_) => "Change Send Level",
            SendField::PreFader(_) => "Change Send Tap",
            SendField::Target(_) => "Change Send Target",
        }
        .into()
    }

    fn apply(&mut self, project: &mut Project) -> Result<ChangeSet, ModelError> {
        let (track, _) = project
            .send_location(self.id)
            .ok_or(ModelError::SendNotFound(self.id))?;
        match self.field {
            SendField::LevelDb(v) => check_finite(v, "send level")?,
            SendField::Target(target) => project.can_route(track, target)?,
            SendField::PreFader(_) => {}
        }
        self.swap(project)
    }

    fn revert(&mut self, project: &mut Project) -> Result<ChangeSet, ModelError> {
        self.swap(project)
    }

    fn merge(&mut self, next: &dyn Command) -> bool {
        next.downcast_ref::<Self>().is_some_and(|n| {
            n.id == self.id && mem::discriminant(&n.field) == mem::discriminant(&self.field)
        })
    }
}

// ---------------------------------------------------------------------------
// Automation

/// Adds an automation lane to a track. Its target must be on that track, and
/// a target can have only one lane.
#[derive(Debug)]
pub struct AddLane {
    pub track: TrackId,
    pub lane: AutomationLane,
}

impl Command for AddLane {
    fn label(&self) -> String {
        "Add Automation Lane".into()
    }

    fn apply(&mut self, project: &mut Project) -> Result<ChangeSet, ModelError> {
        let t = track_of(project, self.track)?;
        let target_ok = match self.lane.target {
            AutomationTarget::Volume | AutomationTarget::Pan => true,
            AutomationTarget::SendLevel { send } => t.sends.iter().any(|s| s.id == send),
            AutomationTarget::DeviceParam { device, .. } => {
                t.devices.iter().any(|d| d.id == device)
            }
        };
        if !target_ok {
            return Err(invalid("automation target isn't on this track"));
        }
        if t.automation.iter().any(|l| l.target == self.lane.target) {
            return Err(invalid("that target already has a lane"));
        }
        check_points(&self.lane.points)?;
        check_ids_free(project, &[self.lane.id.0])?;
        project
            .track_mut(self.track)?
            .automation
            .push(self.lane.clone());
        Ok(ChangeSet::one(Change::LaneList(self.track)))
    }

    fn revert(&mut self, project: &mut Project) -> Result<ChangeSet, ModelError> {
        let t = project.track_mut(self.track)?;
        self.lane = t
            .automation
            .pop()
            .ok_or(ModelError::LaneNotFound(self.lane.id))?;
        Ok(ChangeSet::one(Change::LaneList(self.track)))
    }
}

/// Removes an automation lane.
#[derive(Debug)]
pub struct RemoveLane {
    pub id: LaneId,
    saved: Option<(TrackId, usize, AutomationLane)>,
}

impl RemoveLane {
    pub fn new(id: LaneId) -> Self {
        Self { id, saved: None }
    }
}

impl Command for RemoveLane {
    fn label(&self) -> String {
        "Remove Automation Lane".into()
    }

    fn apply(&mut self, project: &mut Project) -> Result<ChangeSet, ModelError> {
        let (track, i) = project
            .lane_location(self.id)
            .ok_or(ModelError::LaneNotFound(self.id))?;
        let lane = project.track_mut(track)?.automation.remove(i);
        self.saved = Some((track, i, lane));
        Ok(ChangeSet::one(Change::LaneList(track)))
    }

    fn revert(&mut self, project: &mut Project) -> Result<ChangeSet, ModelError> {
        let (track, i, lane) = self
            .saved
            .take()
            .ok_or_else(|| invalid("RemoveLane reverted before apply"))?;
        project.track_mut(track)?.automation.insert(i, lane);
        Ok(ChangeSet::one(Change::LaneList(track)))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum LaneField {
    Enabled(bool),
    /// The full point list; must be strictly increasing in time.
    Points(Vec<AutomationPoint>),
}

/// Sets a lane's points or enabled flag. Consecutive point edits on the same
/// lane (dragging a point) merge.
#[derive(Debug)]
pub struct SetLane {
    pub id: LaneId,
    pub field: LaneField,
}

impl SetLane {
    fn swap(&mut self, project: &mut Project) -> Result<ChangeSet, ModelError> {
        let l = project.lane_mut(self.id)?;
        match &mut self.field {
            LaneField::Enabled(v) => mem::swap(&mut l.enabled, v),
            LaneField::Points(v) => mem::swap(&mut l.points, v),
        }
        Ok(ChangeSet::one(Change::Lane(self.id)))
    }
}

impl Command for SetLane {
    fn label(&self) -> String {
        match self.field {
            LaneField::Enabled(_) => "Toggle Automation",
            LaneField::Points(_) => "Edit Automation",
        }
        .into()
    }

    fn apply(&mut self, project: &mut Project) -> Result<ChangeSet, ModelError> {
        project
            .lane_location(self.id)
            .ok_or(ModelError::LaneNotFound(self.id))?;
        if let LaneField::Points(p) = &self.field {
            check_points(p)?;
        }
        self.swap(project)
    }

    fn revert(&mut self, project: &mut Project) -> Result<ChangeSet, ModelError> {
        self.swap(project)
    }

    fn merge(&mut self, next: &dyn Command) -> bool {
        next.downcast_ref::<Self>().is_some_and(|n| {
            n.id == self.id && mem::discriminant(&n.field) == mem::discriminant(&self.field)
        })
    }
}
