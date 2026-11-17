//! Edit commands the arrange view needs on top of `daw-model`'s own.

use std::mem;
use std::sync::Arc;

use daw_core::time::{SampleTime, TickTime};
use daw_model::{
    Change, ChangeSet, Clip, ClipContent, ClipId, Command, ModelError, Project, TrackId,
};

/// A clip's start, length and (for audio) file offset.
type Extent = (TickTime, TickTime, Option<SampleTime>);

/// Moves a clip's left edge, its right edge or both, keeping its content
/// where it is on the timeline: an audio clip's file offset and a MIDI clip's
/// notes shift to match.
#[derive(Debug)]
pub struct TrimClip {
    pub id: ClipId,
    pub start: TickTime,
    pub length: TickTime,
    /// The new file offset, for audio clips.
    pub offset: Option<SampleTime>,
    /// Where the clip was in its track's list, and its extent.
    old: Option<(usize, Extent)>,
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

    /// Gives `clip` an extent, shifting MIDI notes so they stay put, and
    /// returns the extent it had.
    fn resize(clip: &mut Clip, (start, length, offset): Extent) -> Extent {
        let shift = clip.start.0 - start.0;
        let old_start = mem::replace(&mut clip.start, start);
        let old_length = mem::replace(&mut clip.length, length);
        let old_offset = match &mut clip.content {
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
        (old_start, old_length, old_offset)
    }

    fn changes(&self, track: TrackId) -> ChangeSet {
        [Change::ClipList(track), Change::Clip(self.id)]
            .into_iter()
            .collect()
    }
}

impl Command for TrimClip {
    fn label(&self) -> String {
        "Resize Clip".into()
    }

    fn apply(&mut self, project: &mut Project) -> Result<ChangeSet, ModelError> {
        let (track, index) = project
            .clip_location(self.id)
            .ok_or(ModelError::ClipNotFound(self.id))?;
        let clip = &project.track(track).expect("found above").clips[index];
        if self.length.0 <= 0 || self.start.0 < 0 {
            return Err(ModelError::Invalid(
                "a clip must start at or after 0 and have a positive length".into(),
            ));
        }
        match &clip.content {
            ClipContent::Audio(_) => {
                if self.offset.is_some_and(|o| o.0 < 0) {
                    return Err(ModelError::Invalid(
                        "audio offset must not be negative".into(),
                    ));
                }
            }
            ClipContent::Midi(m) => {
                let shift = clip.start.0 - self.start.0;
                if m.notes.first().is_some_and(|n| n.start.0 + shift < 0) {
                    return Err(ModelError::Invalid(
                        "trimming would cut notes off the clip".into(),
                    ));
                }
            }
        }
        let clips = &mut project.track_mut(track)?.clips;
        let mut arc = clips.remove(index);
        let old = Self::resize(
            Arc::make_mut(&mut arc),
            (self.start, self.length, self.offset),
        );
        // A clip whose start stays keeps its place among the clips that
        // start with it (which of them is on top); a moved one goes on top.
        let at = if old.0 == self.start {
            index
        } else {
            clips.partition_point(|c| c.start <= self.start)
        };
        clips.insert(at, arc);
        self.old = Some((index, old));
        Ok(self.changes(track))
    }

    fn revert(&mut self, project: &mut Project) -> Result<ChangeSet, ModelError> {
        let (index, extent) = self
            .old
            .take()
            .ok_or_else(|| ModelError::Invalid("TrimClip reverted before apply".into()))?;
        let (track, now) = project
            .clip_location(self.id)
            .ok_or(ModelError::ClipNotFound(self.id))?;
        let clips = &mut project.track_mut(track)?.clips;
        let mut arc = clips.remove(now);
        Self::resize(Arc::make_mut(&mut arc), extent);
        clips.insert(index.min(clips.len()), arc);
        Ok(self.changes(track))
    }
}
