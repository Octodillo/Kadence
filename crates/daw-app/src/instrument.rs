//! The instrument of an instrument track, without naming Qt.
//!
//! An instrument track's MIDI player feeds the first device in its chain
//! (see `daw-session`), so that slot is the track's instrument. New
//! instrument tracks start with the built-in synth there, and the user can
//! swap it for any entry of [`InstrumentChoices`] as one undoable edit.
//!
//! [`InstrumentChoices`] is the list the instrument menus show. It holds
//! the built-in instruments; plugin instruments join it with
//! [`InstrumentChoices::push`] once plugins load in the session.

use std::cell::RefCell;
use std::collections::HashSet;

use daw_core::plugin::{PluginDescriptor, PluginFormatKind};
use daw_devices::DeviceCategory;
use daw_model::commands::{AddDevice, AddTrack, Batch, RemoveDevice};
use daw_model::{Command, Device, DeviceSource, Project, Track, TrackId, TrackKind};

/// The instrument a new instrument track starts with.
pub const DEFAULT_INSTRUMENT: &str = "daw.synth";

/// An instrument the user can put on a track.
#[derive(Clone, Debug, PartialEq)]
pub struct InstrumentChoice {
    pub source: DeviceSource,
    /// Display name, also the new device's name.
    pub name: String,
}

impl InstrumentChoice {
    /// A new device of this instrument.
    pub fn device(&self) -> Device {
        Device::new(self.name.clone(), self.source.clone())
    }

    pub fn is_builtin(&self) -> bool {
        matches!(self.source, DeviceSource::Builtin { .. })
    }
}

/// The instruments the menus offer, built-in ones first.
#[derive(Clone, Debug, PartialEq)]
pub struct InstrumentChoices {
    choices: Vec<InstrumentChoice>,
    /// Bumped by every change, so views know to re-read the list.
    revision: u64,
}

impl Default for InstrumentChoices {
    fn default() -> Self {
        Self::builtin()
    }
}

impl InstrumentChoices {
    /// The built-in instruments, in catalog order.
    pub fn builtin() -> Self {
        let choices = daw_devices::devices()
            .iter()
            .filter(|e| e.info.category == DeviceCategory::Instrument)
            .map(|e| InstrumentChoice {
                source: DeviceSource::Builtin {
                    kind: e.info.id.to_owned(),
                },
                name: e.info.name.to_owned(),
            })
            .collect();
        Self {
            choices,
            revision: 0,
        }
    }

    /// Adds a choice at the end, such as a scanned plugin instrument.
    pub fn push(&mut self, choice: InstrumentChoice) {
        self.choices.push(choice);
        self.revision += 1;
    }

    /// Drops every choice that isn't built in, before a rescan.
    pub fn clear_plugins(&mut self) {
        self.choices.retain(InstrumentChoice::is_builtin);
        self.revision += 1;
    }

    pub fn as_slice(&self) -> &[InstrumentChoice] {
        &self.choices
    }

    pub fn get(&self, index: usize) -> Option<&InstrumentChoice> {
        self.choices.get(index)
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// The choice that makes devices like `source`.
    pub fn position(&self, source: &DeviceSource) -> Option<usize> {
        self.choices.iter().position(|c| c.source == *source)
    }

    /// The choice a new instrument track starts with.
    pub fn default_choice(&self) -> Option<&InstrumentChoice> {
        let synth = DeviceSource::Builtin {
            kind: DEFAULT_INSTRUMENT.to_owned(),
        };
        self.position(&synth).and_then(|i| self.get(i))
    }
}

/// Whether a device from `source` makes sound from notes: a built-in
/// instrument, or a plugin the last scan did not find to be an effect (see
/// [`set_plugin_effects`]). A plugin that is not installed counts as an
/// instrument: in the instrument slot it was chosen as one.
pub fn is_instrument(source: &DeviceSource) -> bool {
    match source {
        DeviceSource::Builtin { kind } => daw_devices::devices()
            .iter()
            .any(|e| e.info.id == kind && e.info.category == DeviceCategory::Instrument),
        DeviceSource::Plugin { format, id } => {
            PLUGIN_EFFECTS.with(|e| !e.borrow().contains(&(format.clone(), id.clone())))
        }
    }
}

thread_local! {
    /// Installed plugins that are effects, by format and id.
    static PLUGIN_EFFECTS: RefCell<HashSet<(PluginFormatKind, String)>> =
        RefCell::new(HashSet::new());
}

/// Records which installed plugins are effects, from a plugin scan, so a
/// plugin effect at the start of an instrument track's chain is not taken
/// for its instrument.
pub fn set_plugin_effects<'a>(plugins: impl IntoIterator<Item = &'a PluginDescriptor>) {
    let effects = plugins
        .into_iter()
        .filter(|d| !d.is_instrument)
        .map(|d| (d.format.clone(), d.id.clone()))
        .collect();
    PLUGIN_EFFECTS.with(|e| *e.borrow_mut() = effects);
}

/// The instrument choices for the plugin instruments in `plugins`, named
/// after them. A name more than one plugin has gets its format added, as
/// in "Surge XT (CLAP)", and its vendor too if that is not enough.
pub fn plugin_choices(plugins: &[PluginDescriptor]) -> Vec<InstrumentChoice> {
    let instruments: Vec<&PluginDescriptor> = plugins.iter().filter(|d| d.is_instrument).collect();
    let shared = |d: &PluginDescriptor, same_format: bool| {
        instruments
            .iter()
            .filter(|o| o.name == d.name && (!same_format || o.format == d.format))
            .count()
            > 1
    };
    instruments
        .iter()
        .map(|d| {
            let format = crate::plugins_bridge::format_label(&d.format);
            let name = if shared(d, true) && !d.vendor.is_empty() {
                format!("{} ({}, {format})", d.name, d.vendor)
            } else if shared(d, false) {
                format!("{} ({format})", d.name)
            } else {
                d.name.clone()
            };
            InstrumentChoice {
                source: DeviceSource::Plugin {
                    format: d.format.clone(),
                    id: d.id.clone(),
                },
                name,
            }
        })
        .collect()
}

/// The track's instrument: the first device of an instrument track, if it
/// is one. Effects in that slot (from older projects) don't count.
pub fn instrument_of(track: &Track) -> Option<&Device> {
    if track.kind != TrackKind::Instrument {
        return None;
    }
    track.devices.first().filter(|d| is_instrument(&d.source))
}

/// How many devices at the start of the track's chain are its instrument
/// (0 or 1); effects follow them.
pub fn effects_start(track: &Track) -> usize {
    usize::from(instrument_of(track).is_some())
}

/// Adds `track` at `at`. An instrument track gets `instrument` as its first
/// device in the same undo step.
pub fn add_track(
    at: usize,
    track: Track,
    instrument: Option<&InstrumentChoice>,
) -> Box<dyn Command> {
    let id = track.id;
    let add = Box::new(AddTrack::new(at, track.clone()));
    match instrument {
        Some(choice) if track.kind == TrackKind::Instrument && track.devices.is_empty() => {
            Box::new(Batch::new(
                "Add Track",
                vec![
                    add,
                    Box::new(AddDevice {
                        track: id,
                        index: 0,
                        device: choice.device(),
                    }),
                ],
            ))
        }
        _ => add,
    }
}

/// Puts `choice` in the track's instrument slot, replacing the instrument
/// there, as one undo step. `None` when there's nothing to do: not an
/// instrument track, or it already has that instrument.
pub fn set_instrument(
    project: &Project,
    track: TrackId,
    choice: &InstrumentChoice,
) -> Option<Box<dyn Command>> {
    let t = project.track(track)?;
    if t.kind != TrackKind::Instrument {
        return None;
    }
    let old = instrument_of(t);
    if old.is_some_and(|d| d.source == choice.source) {
        return None;
    }
    let mut commands: Vec<Box<dyn Command>> = Vec::new();
    if let Some(old) = old {
        commands.push(Box::new(RemoveDevice::new(old.id)));
    }
    commands.push(Box::new(AddDevice {
        track,
        index: 0,
        device: choice.device(),
    }));
    Some(Box::new(Batch::new("Change Instrument", commands)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use daw_model::Document;

    fn builtin(kind: &str) -> DeviceSource {
        DeviceSource::Builtin { kind: kind.into() }
    }

    fn plugin(
        format: PluginFormatKind,
        id: &str,
        name: &str,
        instrument: bool,
    ) -> PluginDescriptor {
        PluginDescriptor {
            format,
            id: id.into(),
            name: name.into(),
            vendor: String::new(),
            path: std::path::PathBuf::new(),
            is_instrument: instrument,
            ports: (0, 2),
        }
    }

    #[test]
    fn plugin_choices_are_the_instruments_named_by_format_when_shared() {
        let plugins = [
            plugin(PluginFormatKind::Clap, "surge", "Surge XT", true),
            plugin(PluginFormatKind::Vst3, "surge3", "Surge XT", true),
            plugin(PluginFormatKind::Lv2, "urn:dexed", "Dexed", true),
            plugin(PluginFormatKind::Clap, "gain", "Gain", false),
            PluginDescriptor {
                vendor: "One".into(),
                ..plugin(PluginFormatKind::Clap, "one.pad", "Pad", true)
            },
            PluginDescriptor {
                vendor: "Two".into(),
                ..plugin(PluginFormatKind::Clap, "two.pad", "Pad", true)
            },
        ];
        let names: Vec<String> = plugin_choices(&plugins)
            .into_iter()
            .map(|c| c.name)
            .collect();
        assert_eq!(
            names,
            [
                "Surge XT (CLAP)",
                "Surge XT (VST3)",
                "Dexed",
                "Pad (One, CLAP)",
                "Pad (Two, CLAP)"
            ]
        );
    }

    #[test]
    fn a_scanned_plugin_effect_is_not_an_instrument() {
        let gain = plugin(PluginFormatKind::Clap, "gain", "Gain", false);
        let source = DeviceSource::Plugin {
            format: PluginFormatKind::Clap,
            id: "gain".into(),
        };
        let unknown = DeviceSource::Plugin {
            format: PluginFormatKind::Clap,
            id: "elsewhere".into(),
        };
        assert!(
            is_instrument(&source),
            "unscanned plugins count as instruments"
        );
        set_plugin_effects([&gain]);
        assert!(!is_instrument(&source));
        assert!(is_instrument(&unknown));
        set_plugin_effects([]);
        assert!(is_instrument(&source));
    }

    fn doc_with_track(kind: TrackKind) -> (Document, TrackId) {
        let mut doc = Document::new(Project::new("Test"));
        let choices = InstrumentChoices::builtin();
        let track = Track::new(kind, "Keys");
        let id = track.id;
        doc.execute_boxed(add_track(0, track, choices.default_choice()))
            .unwrap();
        doc.seal();
        (doc, id)
    }

    #[test]
    fn choices_are_the_builtin_instruments() {
        let choices = InstrumentChoices::builtin();
        let sources: Vec<_> = choices.as_slice().iter().map(|c| &c.source).collect();
        assert!(sources.contains(&&builtin("daw.synth")));
        assert!(sources.contains(&&builtin("daw.sampler")));
        assert!(!sources.contains(&&builtin("daw.compressor")));
        assert!(!sources.contains(&&builtin("daw.metronome")));
        assert_eq!(
            choices.default_choice().map(|c| &c.source),
            Some(&builtin(DEFAULT_INSTRUMENT))
        );
    }

    #[test]
    fn plugin_choices_join_and_leave_the_list() {
        let mut choices = InstrumentChoices::builtin();
        let builtins = choices.as_slice().len();
        let r = choices.revision();
        let plugin = InstrumentChoice {
            source: DeviceSource::Plugin {
                format: daw_core::plugin::PluginFormatKind::Clap,
                id: "org.example.synth".into(),
            },
            name: "Example Synth".into(),
        };
        choices.push(plugin.clone());
        assert_eq!(choices.position(&plugin.source), Some(builtins));
        assert!(choices.revision() > r);
        choices.clear_plugins();
        assert_eq!(choices.as_slice().len(), builtins);
    }

    #[test]
    fn a_new_instrument_track_has_the_synth_in_one_undo_step() {
        let (mut doc, id) = doc_with_track(TrackKind::Instrument);
        let t = doc.project().track(id).unwrap();
        assert_eq!(t.devices.len(), 1);
        assert_eq!(t.devices[0].source, builtin("daw.synth"));
        assert_eq!(instrument_of(t).map(|d| d.id), Some(t.devices[0].id));
        assert_eq!(doc.history().undo_label().as_deref(), Some("Add Track"));
        doc.undo().unwrap();
        assert!(doc.project().track(id).is_none(), "one undo removes both");
        assert!(!doc.history().can_undo());
        doc.redo().unwrap();
        assert_eq!(doc.project().track(id).unwrap().devices.len(), 1);
    }

    #[test]
    fn other_tracks_get_no_instrument() {
        for kind in [TrackKind::Audio, TrackKind::Bus] {
            let (doc, id) = doc_with_track(kind);
            assert!(doc.project().track(id).unwrap().devices.is_empty());
        }
    }

    #[test]
    fn swapping_the_instrument_is_one_undoable_edit() {
        let (mut doc, id) = doc_with_track(TrackKind::Instrument);
        let choices = InstrumentChoices::builtin();
        let sampler = &choices.as_slice()[choices.position(&builtin("daw.sampler")).unwrap()];
        let synth = doc.project().track(id).unwrap().devices[0].clone();
        let cmd = set_instrument(doc.project(), id, sampler).unwrap();
        doc.execute_boxed(cmd).unwrap();
        doc.seal();
        let t = doc.project().track(id).unwrap();
        assert_eq!(t.devices.len(), 1, "replaced, not added");
        assert_eq!(t.devices[0].source, builtin("daw.sampler"));
        assert_eq!(
            doc.history().undo_label().as_deref(),
            Some("Change Instrument")
        );
        doc.undo().unwrap();
        let t = doc.project().track(id).unwrap();
        assert_eq!(t.devices.as_slice(), std::slice::from_ref(&synth));
        assert_eq!(doc.history().undo_label().as_deref(), Some("Add Track"));
        // The same instrument again is no edit.
        assert!(set_instrument(doc.project(), id, choices.default_choice().unwrap()).is_none());
    }

    #[test]
    fn the_instrument_goes_before_effects() {
        let mut doc = Document::new(Project::new("Old"));
        // An instrument track from before instruments were added: only an
        // effect.
        let mut track = Track::new(TrackKind::Instrument, "Keys");
        track
            .devices
            .push(Device::new("Compressor", builtin("daw.compressor")));
        let id = track.id;
        doc.execute(AddTrack::new(0, track)).unwrap();
        let t = doc.project().track(id).unwrap();
        assert!(instrument_of(t).is_none());
        assert_eq!(effects_start(t), 0);
        let choices = InstrumentChoices::builtin();
        let cmd = set_instrument(doc.project(), id, choices.default_choice().unwrap()).unwrap();
        doc.execute_boxed(cmd).unwrap();
        let t = doc.project().track(id).unwrap();
        let kinds: Vec<_> = t.devices.iter().map(|d| d.source.clone()).collect();
        assert_eq!(kinds, [builtin("daw.synth"), builtin("daw.compressor")]);
        assert_eq!(effects_start(t), 1);
    }

    #[test]
    fn audio_tracks_have_no_instrument_slot() {
        let (doc, id) = doc_with_track(TrackKind::Audio);
        let choices = InstrumentChoices::builtin();
        assert!(set_instrument(doc.project(), id, choices.default_choice().unwrap()).is_none());
    }

    /// Peak level of `track` while the stub engine plays `doc` for up to
    /// `wait`, stopping early once it is loud.
    /// Also returns whether playback moved, so silence means silence and
    /// not a backend that never ran.
    fn stub_peak(doc: &mut Document, track: TrackId, wait: std::time::Duration) -> (f32, bool) {
        use crate::engine::{EngineHandle, StubEngine};
        let mut engine = StubEngine::default();
        let session = engine.session_mut().unwrap();
        session.sync(&doc.snapshot()).unwrap();
        let meter = session.track_meter(track).expect("the track has a strip");
        engine.play();
        let start = std::time::Instant::now();
        let mut peak = 0.0f32;
        while start.elapsed() < wait && peak < 1e-3 {
            std::thread::sleep(std::time::Duration::from_millis(20));
            peak = meter.take().iter().fold(peak, |m, v| m.max(v.abs()));
        }
        let moved = engine.session().unwrap().position() > daw_core::time::TickTime(0);
        engine.stop();
        (peak, moved)
    }

    /// A clip holding middle C for its whole bar, at the start of the song.
    fn add_notes(doc: &mut Document, track: TrackId) {
        use daw_core::time::{PPQ, TickTime};
        use daw_model::commands::AddClip;
        use daw_model::{Clip, ClipContent, MidiClip, Note, NoteUid};
        let note = Note {
            id: NoteUid::new(),
            start: TickTime(0),
            length: TickTime(4 * PPQ),
            channel: 0,
            key: 60,
            velocity: 0.8,
        };
        let clip = Clip::new(
            "notes",
            TickTime(0),
            TickTime(4 * PPQ),
            ClipContent::Midi(MidiClip { notes: vec![note] }),
        );
        doc.execute(AddClip::new(track, clip)).unwrap();
    }

    #[test]
    fn a_new_instrument_track_plays_its_notes_through_the_stub_engine() {
        let wait = std::time::Duration::from_secs(5);
        let (mut doc, id) = doc_with_track(TrackKind::Instrument);
        add_notes(&mut doc, id);
        let (peak, _) = stub_peak(&mut doc, id, wait);
        assert!(peak > 1e-3, "the synth sounds: peak {peak}");

        // Without the instrument (as before), the same notes are silent.
        let mut doc = Document::new(Project::new("Test"));
        let track = Track::new(TrackKind::Instrument, "Keys");
        let id = track.id;
        doc.execute(AddTrack::new(0, track)).unwrap();
        add_notes(&mut doc, id);
        let (peak, moved) = stub_peak(&mut doc, id, std::time::Duration::from_millis(500));
        assert!(moved, "the stub engine played");
        assert!(peak < 1e-6, "no instrument, no sound: peak {peak}");
    }
}
