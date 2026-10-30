//! Persistent user settings.
//!
//! Values live in KConfig (`~/.config/kadencerc`) through [`KConfigStore`].
//! The load and save logic is written against [`SettingsStore`] so it can be
//! tested with an in-memory map.

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[derive(Default)]
    struct MemoryStore(HashMap<(String, String), String>);

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
}
