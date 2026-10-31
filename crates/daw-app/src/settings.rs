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
    store
        .read(PLUGINS, "ExtraFolders")
        .map(|v| {
            v.lines()
                .filter(|l| !l.is_empty())
                .map(PathBuf::from)
                .collect()
        })
        .unwrap_or_default()
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
