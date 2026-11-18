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
        // The same checks a project file gets, so a trim can't save one
        // that will not load.
        daw_model::check_tick(self.start, "clip start")?;
        daw_model::check_length(self.length, "clip length")?;
        match &clip.content {
            ClipContent::Audio(_) => {
                if let Some(o) = self.offset {
                    daw_model::check_audio_offset(o)?;
                }
            }
            ClipContent::Midi(m) => {
                let shift = clip.start.0 - self.start.0;
                // Sorted by start: the first and last are the extremes.
                for n in m.notes.first().into_iter().chain(m.notes.last()) {
                    daw_model::check_tick(TickTime(n.start.0 + shift), "note start").map_err(
                        |_| ModelError::Invalid("trimming would cut notes off the clip".into()),
                    )?;
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
