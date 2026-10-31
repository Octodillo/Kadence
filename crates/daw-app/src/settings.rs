//! Persistent user settings.
//!
//! Values live in KConfig (`~/.config/kadencerc`) through [`KConfigStore`].
//! The load and save logic is written against [`SettingsStore`] so it can be
//! tested with an in-memory map.

use daw_session::MetronomeSettings;
use std::path::{Path, PathBuf};

use crate::engine::AudioConfig;
use crate::ffi;

/// A flat group/key/value store, like KConfig.
pub trait SettingsStore {
    /// The stored value, or `None` when the key is missing or empty.
    fn read(&self, group: &str, key: &str) -> Option<String>;
    fn write(&mut self, group: &str, key: &str, value: &str);
    /// Make earlier writes durable, all at once.
    fn sync(&mut self) {}
}

/// The application's KConfig file. Writes reach disk on [`SettingsStore::sync`].
#[derive(Debug, Default)]
pub struct KConfigStore;

impl SettingsStore for KConfigStore {
    fn read(&self, group: &str, key: &str) -> Option<String> {
        let value = ffi::config_read(group, key);
        (!value.is_empty()).then_some(value)
    }

    fn write(&mut self, group: &str, key: &str, value: &str) {
        ffi::config_write(group, key, value);
    }

    fn sync(&mut self) {
        ffi::config_sync();
    }
}

const AUDIO: &str = "Audio";

/// Read the saved audio configuration, falling back to defaults per key.
pub fn load_audio_config(store: &dyn SettingsStore) -> AudioConfig {
    let defaults = AudioConfig::default();
    let number = |key: &str, fallback: u32| {
        store
            .read(AUDIO, key)
            .and_then(|v| v.parse().ok())
            .unwrap_or(fallback)
    };
    AudioConfig {
        device: store.read(AUDIO, "Device"),
        sample_rate: number("SampleRate", defaults.sample_rate),
        buffer_size: number("BufferSize", defaults.buffer_size),
    }
}

pub fn save_audio_config(store: &mut dyn SettingsStore, config: &AudioConfig) {
    store.write(AUDIO, "Device", config.device.as_deref().unwrap_or(""));
    store.write(AUDIO, "SampleRate", &config.sample_rate.to_string());
    store.write(AUDIO, "BufferSize", &config.buffer_size.to_string());
    store.sync();
}

const MIDI: &str = "MIDI";
const GENERAL: &str = "General";

/// A list in one config value: one item per line (port names and paths
/// have no line breaks).
fn read_list(store: &dyn SettingsStore, group: &str, key: &str) -> Vec<String> {
    store
        .read(group, key)
        .map(|v| {
            v.lines()
                .filter(|l| !l.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn read_bool(store: &dyn SettingsStore, group: &str, key: &str, fallback: bool) -> bool {
    match store.read(group, key).as_deref() {
        Some("true") => true,
        Some("false") => false,
        _ => fallback,
    }
}

/// Which MIDI sources get a port of their own (see
/// `daw_session::host::AudioHost::set_midi_inputs`). Only tracks that take
/// every input hear sources that don't.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MidiSettings {
    /// Sources the user turned on, by port name.
    pub enabled: Vec<String>,
    /// Sources the user turned off.
    pub disabled: Vec<String>,
    /// Hardware the user has not decided about is used.
    pub use_new_devices: bool,
}

impl Default for MidiSettings {
    fn default() -> Self {
        Self {
            enabled: Vec::new(),
            disabled: Vec::new(),
            use_new_devices: true,
        }
    }
}

impl MidiSettings {
    /// Whether `source` is used: the user's choice, else whether it is
    /// hardware and new devices are used. Programs are used only when
    /// turned on.
    pub fn uses(&self, source: &str, is_physical: bool) -> bool {
        if self.enabled.iter().any(|s| s == source) {
            true
        } else if self.disabled.iter().any(|s| s == source) {
            false
        } else {
            self.use_new_devices && is_physical
        }
    }

    pub fn set_used(&mut self, source: &str, used: bool) {
        self.enabled.retain(|s| s != source);
        self.disabled.retain(|s| s != source);
        if used {
            self.enabled.push(source.to_owned());
        } else {
            self.disabled.push(source.to_owned());
        }
    }

    /// The sources to give ports to, given the ones there now (name, is
    /// hardware), the ones that have ports already, and the ones tracks
    /// listen to. Sources turned on keep their port while unplugged, and
    /// so do automatic ones a track listens to, so the track keeps its
    /// input across a replug. Other automatic ones lose it, so devices
    /// that come back under a new name don't pile up ports.
    pub fn wanted(
        &self,
        present: &[(String, bool)],
        current: &[String],
        listened: &[String],
    ) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut add = |s: &str| {
            if !out.iter().any(|o| o == s) {
                out.push(s.to_owned());
            }
        };
        for s in current {
            let here = present.iter().find(|(n, _)| n == s);
            let kept = here.is_some() || listened.contains(s) || self.enabled.contains(s);
            if kept && self.uses(s, here.is_none_or(|p| p.1)) {
                add(s);
            }
        }
        for s in &self.enabled {
            add(s);
        }
        for (s, physical) in present {
            if self.uses(s, *physical) {
                add(s);
            }
        }
        out
    }
}

pub fn load_midi_settings(store: &dyn SettingsStore) -> MidiSettings {
    MidiSettings {
        enabled: read_list(store, MIDI, "Enabled"),
        disabled: read_list(store, MIDI, "Disabled"),
        use_new_devices: read_bool(store, MIDI, "UseNewDevices", true),
    }
}

pub fn save_midi_settings(store: &mut dyn SettingsStore, settings: &MidiSettings) {
    store.write(MIDI, "Enabled", &settings.enabled.join("\n"));
    store.write(MIDI, "Disabled", &settings.disabled.join("\n"));
    store.write(
        MIDI,
        "UseNewDevices",
        if settings.use_new_devices {
            "true"
        } else {
            "false"
        },
    );
    store.sync();
}

/// Settings that are not about audio or MIDI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GeneralSettings {
    /// Where Open and Save As start; `None` leaves it to the dialogs.
    pub project_folder: Option<String>,
    /// Minutes between autosaves of a changed project; 0 turns autosave off.
    pub autosave_minutes: u32,
}

impl Default for GeneralSettings {
    fn default() -> Self {
        Self {
            project_folder: None,
            autosave_minutes: 1,
        }
    }
}

const METRONOME: &str = "Metronome";

/// The click and the count-in before recording. A playing preference, not
/// part of the project.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ClickSettings {
    pub metronome: MetronomeSettings,
    /// Bars counted in before recording; 0 for none.
    pub count_in_bars: u32,
}

impl ClickSettings {
    /// The longest count-in offered.
    pub const MAX_COUNT_IN: u32 = 2;
    /// The quietest and loudest click, in dB.
    pub const MIN_VOLUME_DB: f64 = -40.0;
    pub const MAX_VOLUME_DB: f64 = 6.0;

    /// These settings with every value in range.
    pub fn clamped(self) -> Self {
        let volume = self.metronome.volume_db;
        let volume = if volume.is_finite() {
            volume
        } else {
            MetronomeSettings::default().volume_db
        };
        Self {
            metronome: MetronomeSettings {
                volume_db: volume.clamp(Self::MIN_VOLUME_DB, Self::MAX_VOLUME_DB),
                ..self.metronome
            },
            count_in_bars: self.count_in_bars.min(Self::MAX_COUNT_IN),
        }
    }
}

/// Longest autosave interval the settings offer, in minutes.
pub const MAX_AUTOSAVE_MINUTES: u32 = 60;

pub fn load_general_settings(store: &dyn SettingsStore) -> GeneralSettings {
    let defaults = GeneralSettings::default();
    GeneralSettings {
        project_folder: store.read(GENERAL, "ProjectFolder"),
        autosave_minutes: store
            .read(GENERAL, "AutosaveMinutes")
            .and_then(|v| v.parse().ok())
            .filter(|&m| m <= MAX_AUTOSAVE_MINUTES)
            .unwrap_or(defaults.autosave_minutes),
    }
}

pub fn save_general_settings(store: &mut dyn SettingsStore, settings: &GeneralSettings) {
    store.write(
        GENERAL,
        "ProjectFolder",
        settings.project_folder.as_deref().unwrap_or(""),
    );
    store.write(
        GENERAL,
        "AutosaveMinutes",
        &settings.autosave_minutes.to_string(),
    );
    store.sync();
}

/// Read the saved click settings, falling back to defaults per key.
pub fn load_click_settings(store: &dyn SettingsStore) -> ClickSettings {
    let d = ClickSettings::default();
    let read = |key: &str| store.read(METRONOME, key);
    ClickSettings {
        metronome: MetronomeSettings {
            enabled: read("Enabled").map_or(d.metronome.enabled, |v| v == "true"),
            volume_db: read("VolumeDb")
                .and_then(|v| v.parse().ok())
                .unwrap_or(d.metronome.volume_db),
        },
        count_in_bars: read("CountInBars")
            .and_then(|v| v.parse().ok())
            .unwrap_or(d.count_in_bars),
    }
    .clamped()
}

pub fn save_click_settings(store: &mut dyn SettingsStore, click: &ClickSettings) {
    store.write(METRONOME, "Enabled", &click.metronome.enabled.to_string());
    store.write(
        METRONOME,
        "VolumeDb",
        &click.metronome.volume_db.to_string(),
    );
    store.write(METRONOME, "CountInBars", &click.count_in_bars.to_string());
    store.sync();
}

const LAYOUT: &str = "Layout";

/// A size the user gave part of the window (a splitter, a column width),
/// under `key`; `fallback` when none was saved or it isn't a number.
pub fn load_layout_value(store: &dyn SettingsStore, key: &str, fallback: f64) -> f64 {
    store
        .read(LAYOUT, key)
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|v| v.is_finite())
        .unwrap_or(fallback)
}

/// Remembers a size for [`load_layout_value`]; a value that isn't a
/// number is ignored.
pub fn save_layout_value(store: &mut dyn SettingsStore, key: &str, value: f64) {
    if value.is_finite() {
        store.write(LAYOUT, key, &value.to_string());
        store.sync();
    }
}

const RECENT: &str = "RecentProjects";
/// How many projects Open Recent lists.
pub const RECENT_KEPT: usize = 10;

fn recent_key(i: usize) -> String {
    format!("Project{}", i + 1)
}

/// The recently used project bundles, newest first. Paths are stored as
/// text, so a bundle whose path is not UTF-8 is not remembered.
pub fn load_recent(store: &dyn SettingsStore) -> Vec<PathBuf> {
    let mut list: Vec<PathBuf> = Vec::new();
    for i in 0..RECENT_KEPT {
        if let Some(p) = store.read(RECENT, &recent_key(i)).map(PathBuf::from)
            && !list.contains(&p)
        {
            list.push(p);
        }
    }
    list
}

fn save_recent(store: &mut dyn SettingsStore, list: &[PathBuf]) {
    for i in 0..RECENT_KEPT {
        let value = list.get(i).and_then(|p| p.to_str()).unwrap_or("");
        store.write(RECENT, &recent_key(i), value);
    }
    store.sync();
}

/// Puts `bundle` first in the recent list and returns the new list.
pub fn add_recent(store: &mut dyn SettingsStore, bundle: &Path) -> Vec<PathBuf> {
    let mut list = load_recent(store);
    list.retain(|p| p != bundle);
    if bundle.to_str().is_some() {
        list.insert(0, bundle.to_path_buf());
    }
    list.truncate(RECENT_KEPT);
    save_recent(store, &list);
    list
}

/// Drops `bundle` from the recent list and returns the new list.
pub fn remove_recent(store: &mut dyn SettingsStore, bundle: &Path) -> Vec<PathBuf> {
    let mut list = load_recent(store);
    list.retain(|p| p != bundle);
    save_recent(store, &list);
    list
}

/// A settings store in memory, for tests and for runs that must not touch
/// the user's config.
#[derive(Debug, Default)]
pub struct MemoryStore(std::collections::HashMap<(String, String), String>);

impl SettingsStore for MemoryStore {
    fn read(&self, group: &str, key: &str) -> Option<String> {
        self.0
            .get(&(group.to_owned(), key.to_owned()))
            .filter(|v| !v.is_empty())
            .cloned()
    }

    fn write(&mut self, group: &str, key: &str, value: &str) {
        self.0
            .insert((group.to_owned(), key.to_owned()), value.to_owned());
    }
}

const PLUGINS: &str = "Plugins";

/// Folders the user added to the plugin search, one path per line.
pub fn load_plugin_folders(store: &dyn SettingsStore) -> Vec<PathBuf> {
    read_list(store, PLUGINS, "ExtraFolders")
        .into_iter()
        .map(PathBuf::from)
        .collect()
}

pub fn save_plugin_folders(store: &mut dyn SettingsStore, folders: &[PathBuf]) {
    let value: Vec<String> = folders
        .iter()
        .map(|f| f.to_string_lossy().into_owned())
        .collect();
    store.write(PLUGINS, "ExtraFolders", &value.join("\n"));
    store.sync();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_values_fall_back_when_missing_or_broken() {
        let mut store = MemoryStore::default();
        assert_eq!(load_layout_value(&store, "MixerStripWidth", 7.0), 7.0);
        save_layout_value(&mut store, "MixerStripWidth", 9.5);
        assert_eq!(load_layout_value(&store, "MixerStripWidth", 7.0), 9.5);
        save_layout_value(&mut store, "MixerStripWidth", f64::NAN);
        assert_eq!(load_layout_value(&store, "MixerStripWidth", 7.0), 9.5);
        store.write(LAYOUT, "MixerStripWidth", "wide");
        assert_eq!(load_layout_value(&store, "MixerStripWidth", 7.0), 7.0);
        store.write(LAYOUT, "MixerStripWidth", "inf");
        assert_eq!(load_layout_value(&store, "MixerStripWidth", 7.0), 7.0);
    }

    #[test]
    fn empty_store_gives_defaults() {
        assert_eq!(
            load_audio_config(&MemoryStore::default()),
            AudioConfig::default()
        );
    }

    #[test]
    fn round_trip() {
        let mut store = MemoryStore::default();
        let config = AudioConfig {
            device: Some("hw:1".into()),
            sample_rate: 96_000,
            buffer_size: 64,
        };
        save_audio_config(&mut store, &config);
        assert_eq!(load_audio_config(&store), config);

        let default_device = AudioConfig {
            device: None,
            ..config
        };
        save_audio_config(&mut store, &default_device);
        assert_eq!(load_audio_config(&store), default_device);
    }

    #[test]
    fn midi_settings_round_trip_and_default_to_using_hardware() {
        let mut store = MemoryStore::default();
        let defaults = load_midi_settings(&store);
        assert_eq!(defaults, MidiSettings::default());
        assert!(defaults.uses("Midi-Bridge:Keys", true));
        assert!(
            !defaults.uses("seq:out", false),
            "programs only when chosen"
        );

        let mut s = defaults;
        s.set_used("seq:out", true);
        s.set_used("Midi-Bridge:Keys", false);
        s.set_used("Midi-Bridge:Pads", true);
        s.set_used("Midi-Bridge:Pads", false);
        s.use_new_devices = false;
        save_midi_settings(&mut store, &s);
        let loaded = load_midi_settings(&store);
        assert_eq!(loaded, s);
        assert_eq!(loaded.enabled, ["seq:out"]);
        assert_eq!(loaded.disabled, ["Midi-Bridge:Keys", "Midi-Bridge:Pads"]);
        assert!(!loaded.uses("Midi-Bridge:New", true));
    }

    #[test]
    fn wanted_midi_sources_keep_their_ports_while_unplugged() {
        let mut s = MidiSettings::default();
        s.set_used("seq:out", true);
        s.set_used("hw:off", false);
        let present = [
            ("hw:keys".to_owned(), true),
            ("hw:off".to_owned(), true),
            ("other:out".to_owned(), false),
        ];
        let listened = ["hw:keys".to_owned()];
        let wanted = s.wanted(&present, &[], &listened);
        assert_eq!(wanted, ["seq:out", "hw:keys"]);
        // Keys unplugged: still wanted, since a track listens to it.
        let wanted = s.wanted(&present[1..], &wanted, &listened);
        assert_eq!(wanted, ["seq:out", "hw:keys"]);
        // ... but not when no track does.
        assert_eq!(s.wanted(&present[1..], &wanted, &[]), ["seq:out"]);
        // Turned off: dropped.
        s.set_used("hw:keys", false);
        assert_eq!(s.wanted(&present, &wanted, &listened), ["seq:out"]);
    }

    #[test]
    fn general_settings_round_trip() {
        let mut store = MemoryStore::default();
        assert_eq!(load_general_settings(&store), GeneralSettings::default());
        let s = GeneralSettings {
            project_folder: Some("/home/me/Music/Projects".into()),
            autosave_minutes: 0,
        };
        save_general_settings(&mut store, &s);
        assert_eq!(load_general_settings(&store), s);
        store.write(GENERAL, "AutosaveMinutes", "9999");
        assert_eq!(load_general_settings(&store).autosave_minutes, 1);
    }

    #[test]
    fn garbage_falls_back_per_key() {
        let mut store = MemoryStore::default();
        store.write(AUDIO, "SampleRate", "fast");
        store.write(AUDIO, "BufferSize", "128");
        let config = load_audio_config(&store);
        assert_eq!(config.sample_rate, AudioConfig::default().sample_rate);
        assert_eq!(config.buffer_size, 128);
    }

    #[test]
    fn click_settings_round_trip_and_clamp() {
        let mut store = MemoryStore::default();
        assert_eq!(load_click_settings(&store), ClickSettings::default());
        let click = ClickSettings {
            metronome: MetronomeSettings {
                enabled: true,
                volume_db: -12.5,
            },
            count_in_bars: 2,
        };
        save_click_settings(&mut store, &click);
        assert_eq!(load_click_settings(&store), click);

        store.write(METRONOME, "VolumeDb", "loud");
        store.write(METRONOME, "CountInBars", "9");
        let loaded = load_click_settings(&store);
        assert_eq!(loaded.metronome.volume_db, -6.0);
        assert_eq!(loaded.count_in_bars, 2);
        store.write(METRONOME, "VolumeDb", "-500");
        assert_eq!(load_click_settings(&store).metronome.volume_db, -40.0);
    }

    #[test]
    fn recent_list_is_newest_first_without_duplicates() {
        let mut store = MemoryStore::default();
        assert!(load_recent(&store).is_empty());
        add_recent(&mut store, Path::new("/songs/a"));
        add_recent(&mut store, Path::new("/songs/b"));
        let list = add_recent(&mut store, Path::new("/songs/a"));
        assert_eq!(
            list,
            vec![PathBuf::from("/songs/a"), PathBuf::from("/songs/b")]
        );
        assert_eq!(load_recent(&store), list);
    }

    #[test]
    fn recent_list_keeps_the_newest_ten() {
        let mut store = MemoryStore::default();
        for i in 0..15 {
            add_recent(&mut store, &PathBuf::from(format!("/songs/{i}")));
        }
        let list = load_recent(&store);
        assert_eq!(list.len(), RECENT_KEPT);
        assert_eq!(list[0], PathBuf::from("/songs/14"));
        assert_eq!(list[9], PathBuf::from("/songs/5"));
    }

    #[test]
    fn removing_from_the_recent_list_closes_the_gap() {
        let mut store = MemoryStore::default();
        for name in ["/a", "/b", "/c"] {
            add_recent(&mut store, Path::new(name));
        }
        let list = remove_recent(&mut store, Path::new("/b"));
        assert_eq!(list, vec![PathBuf::from("/c"), PathBuf::from("/a")]);
        assert_eq!(load_recent(&store), list);
        // The freed slot is cleared, not left holding the last entry twice.
        assert_eq!(store.read(RECENT, "Project3"), None);
    }

    #[test]
    fn recent_list_skips_gaps_and_repeats_in_the_file() {
        let mut store = MemoryStore::default();
        store.write(RECENT, "Project1", "/a");
        store.write(RECENT, "Project3", "/b");
        store.write(RECENT, "Project4", "/a");
        assert_eq!(
            load_recent(&store),
            vec![PathBuf::from("/a"), PathBuf::from("/b")]
        );
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_paths_are_not_remembered() {
        use std::os::unix::ffi::OsStrExt;
        let mut store = MemoryStore::default();
        add_recent(&mut store, Path::new("/a"));
        let odd = Path::new(std::ffi::OsStr::from_bytes(b"/so\xffng"));
        assert_eq!(add_recent(&mut store, odd), vec![PathBuf::from("/a")]);
    }

    #[test]
    fn plugin_folders_round_trip() {
        let mut store = MemoryStore::default();
        assert!(load_plugin_folders(&store).is_empty());
        let folders = vec![
            PathBuf::from("/opt/plugins"),
            PathBuf::from("/home/a b/clap"),
        ];
        save_plugin_folders(&mut store, &folders);
        assert_eq!(load_plugin_folders(&store), folders);
        save_plugin_folders(&mut store, &[]);
        assert!(load_plugin_folders(&store).is_empty());
    }
}
