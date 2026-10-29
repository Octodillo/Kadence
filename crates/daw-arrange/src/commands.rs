//! Edit commands the arrange view needs on top of `daw-model`'s own.

use std::mem;

use daw_core::time::{SampleTime, TickTime};
use daw_model::{
    Change, ChangeSet, ClipContent, ClipId, Command, ModelError, Project, TrackId,
};

/// Several commands that undo as one step and that merge with the next group
/// of the same kind on the same clips, so a drag of a whole selection is one
/// undo step however many pointer moves it took.
#[derive(Debug)]
pub struct Group {
    label: String,
    key: Vec<ClipId>,
    items: Vec<Box<dyn Command>>,
}

impl Group {
    /// `key` names what the group edits; only groups with the same label and
    /// key merge.
    pub fn new(label: impl Into<String>, key: Vec<ClipId>, items: Vec<Box<dyn Command>>) -> Self {
        Self {
            label: label.into(),
            key,
            items,
        }
    }
}

impl Command for Group {
    fn label(&self) -> String {
        self.label.clone()
    }

    fn apply(&mut self, project: &mut Project) -> Result<ChangeSet, ModelError> {
        let mut changes = ChangeSet::new();
        for i in 0..self.items.len() {
            match self.items[i].apply(project) {
                Ok(c) => changes.extend(c),
                Err(e) => {
                    for done in self.items[..i].iter_mut().rev() {
                        done.revert(project)?;
                    }
                    return Err(e);
                }
            }
        }
        Ok(changes)
    }

    fn revert(&mut self, project: &mut Project) -> Result<ChangeSet, ModelError> {
        let mut changes = ChangeSet::new();
        for item in self.items.iter_mut().rev() {
            changes.extend(item.revert(project)?);
        }
        Ok(changes)
    }

    fn merge(&mut self, next: &dyn Command) -> bool {
        let Some(next) = next.downcast_ref::<Self>() else {
            return false;
        };
        if next.label != self.label
            || next.key != self.key
            || next.items.len() != self.items.len()
            || self
                .items
                .iter()
                .zip(&next.items)
                .any(|(a, b)| a.label() != b.label())
        {
            return false;
        }
        // Every pair edits the same object in the same way, so each merges.
        self.items
            .iter_mut()
            .zip(&next.items)
            .all(|(a, b)| a.merge(b.as_ref()))
    }
}

/// Moves a clip's left edge, its right edge or both, keeping its content
/// where it is on the timeline: an audio clip's file offset and a MIDI clip's
/// notes shift to match. Consecutive trims of the same clip (a drag) merge.
#[derive(Debug)]
pub struct TrimClip {
    pub id: ClipId,
    pub start: TickTime,
    pub length: TickTime,
    /// The new file offset, for audio clips.
    pub offset: Option<SampleTime>,
    old: Option<(TickTime, TickTime, Option<SampleTime>)>,
}

impl TrimClip {
    pub fn new(id: ClipId, start: TickTime, length: TickTime, offset: Option<SampleTime>) -> Self {
        Self {
            id,
            start,
            length,
            offset,
            old: None,
        }
    }

    /// Sets start, length and offset, shifting MIDI notes so they stay put.
    fn set(
        project: &mut Project,
        id: ClipId,
        start: TickTime,
        length: TickTime,
        offset: Option<SampleTime>,
    ) -> Result<(TrackId, (TickTime, TickTime, Option<SampleTime>)), ModelError> {
        let (track, index) = project
            .clip_location(id)
            .ok_or(ModelError::ClipNotFound(id))?;
        let clip = &project.track(track).expect("found above").clips[index];
        if length.0 <= 0 || start.0 < 0 {
            return Err(ModelError::Invalid(
                "a clip must start at or after 0 and have a positive length".into(),
            ));
        }
        let shift = clip.start.0 - start.0;
        match &clip.content {
            ClipContent::Audio(_) => {
                if offset.is_some_and(|o| o.0 < 0) {
                    return Err(ModelError::Invalid(
                        "audio offset must not be negative".into(),
                    ));
                }
            }
            ClipContent::Midi(m) => {
                if m.notes.first().is_some_and(|n| n.start.0 + shift < 0) {
                    return Err(ModelError::Invalid(
                        "trimming would cut notes off the clip".into(),
                    ));
                }
            }
        }
        let t = project.track_mut(track)?;
        let mut arc = t.clips.remove(index);
        let c = std::sync::Arc::make_mut(&mut arc);
        let old_start = mem::replace(&mut c.start, start);
        let old_length = mem::replace(&mut c.length, length);
        let old_offset = match &mut c.content {
            ClipContent::Audio(a) => {
                let old = a.offset;
                if let Some(o) = offset {
                    a.offset = o;
                }
                Some(old)
            }
            ClipContent::Midi(m) => {
                if shift != 0 {
                    for n in &mut m.notes {
                        n.start.0 += shift;
                    }
                }
                None
            }
        };
        let at = t.clips.partition_point(|x| x.start <= start);
        t.clips.insert(at, arc);
        Ok((track, (old_start, old_length, old_offset)))
    }
}

impl Command for TrimClip {
    fn label(&self) -> String {
        "Resize Clip".into()
    }

    fn apply(&mut self, project: &mut Project) -> Result<ChangeSet, ModelError> {
        let (track, old) = Self::set(project, self.id, self.start, self.length, self.offset)?;
        self.old = Some(old);
        Ok([Change::ClipList(track), Change::Clip(self.id)]
            .into_iter()
            .collect())
    }

    fn revert(&mut self, project: &mut Project) -> Result<ChangeSet, ModelError> {
        let (start, length, offset) = self
            .old
            .take()
            .ok_or_else(|| ModelError::Invalid("TrimClip reverted before apply".into()))?;
        let (track, _) = Self::set(project, self.id, start, length, offset)?;
        Ok([Change::ClipList(track), Change::Clip(self.id)]
            .into_iter()
            .collect())
    }

    fn merge(&mut self, next: &dyn Command) -> bool {
        match next.downcast_ref::<Self>() {
            Some(n) if n.id == self.id => {
                self.start = n.start;
                self.length = n.length;
                self.offset = n.offset;
                true
            }
            _ => false,
        }
    }
}
