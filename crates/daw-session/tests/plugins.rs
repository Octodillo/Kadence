//! Plays projects with a plugin device: the `daw-clap-test-plugin` CLAP, a gain whose state is
//! its gain, loaded in a sandbox host through the session's plugin loader.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use assert_no_alloc::{AllocDisabler, assert_no_alloc, violation_count};
use daw_core::event::EventList;
use daw_core::plugin::PluginFormatKind;
use daw_core::time::{PPQ, SampleTime, TickTime};
use daw_engine::{EngineConfig, EngineProcessor};
use daw_media::{AudioData, WavFormat, write_wav};
use daw_model::commands::{
    AddClip, AddDevice, AddLane, DeviceField, RemoveDevice, SetDevice, SetParam,
};
use daw_model::{
    AudioClip, AutomationLane, AutomationPoint, AutomationTarget, Clip, ClipContent, Device,
    DeviceId, DeviceSource, Document, Project, Track, TrackId, TrackKind,
};
use daw_plugin_host::{HostLauncher, SandboxConfig};
use daw_session::catalog::{Filter, PluginCatalog, PluginKind};
use daw_session::{PluginStatus, SandboxLoader, Session, SessionConfig, Warning};

#[global_allocator]
static ALLOC: AllocDisabler = AllocDisabler;

const RATE: f64 = 48_000.0;
const BLOCK: usize = 256;
const ID: &str = "org.daw.test.gain";
const GAIN: u32 = 1;
const LEVEL: f32 = 0.5;

/// Builds the test plugin and the sandbox host in their own target directory (shared with the
/// sandbox's tests) and returns a folder holding the plugin, and the host binary.
fn binaries() -> &'static (PathBuf, PathBuf) {
    static PATHS: OnceLock<(PathBuf, PathBuf)> = OnceLock::new();
    PATHS.get_or_init(|| {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let target = root.join("target").join("clap-test-plugin");
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        // The same build as every other test that uses this target directory: a
        // different package set unifies features differently and rebuilds the
        // plugin while another test process is copying or loading it.
        let status = Command::new(cargo)
            .args([
                "build",
                "--quiet",
                "--package",
                "daw-clap-test-plugin",
                "--package",
                "daw-plugin-host",
                "--bin",
                "daw-plugin-host",
                "--lib",
                "--target-dir",
            ])
            .arg(&target)
            .current_dir(&root)
            .status()
            .expect("run cargo");
        assert!(status.success(), "building the test plugin and host failed");
        let dir = target.join(format!("session-plugins-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let lib = target.join("debug").join("libdaw_clap_test_plugin.so");
        std::fs::copy(&lib, dir.join("test-gain.clap")).expect("copy the test plugin");
        (dir, target.join("debug").join("daw-plugin-host"))
    })
}

/// A loader that finds only the test plugin, set up as an export's.
fn export_loader() -> SandboxLoader {
    loader_with(daw_session::export::offline_sandbox)
}

/// A loader that finds only the test plugin.
fn loader() -> SandboxLoader {
    loader_with(|c| c)
}

fn loader_with(adjust: impl FnOnce(SandboxConfig) -> SandboxConfig) -> SandboxLoader {
    let (plugins, host) = binaries();
    let mut config = SandboxConfig::new(HostLauncher::new(host, Vec::<String>::new()));
    // CI machines are slow and the host is a debug build.
    config.process_timeout = Some(Duration::from_secs(5));
    config.rt_priority = None;
    let mut loader = SandboxLoader::new(adjust(config));
    let registry = loader.registry_mut();
    registry.set_search_paths(PluginFormatKind::Clap, vec![plugins.clone()]);
    registry.set_search_paths(PluginFormatKind::Lv2, vec![]);
    registry.set_search_paths(PluginFormatKind::Vst3, vec![]);
    loader
}

/// A catalog that scans only `extra` (as the user's added folders), with the sandbox host as
/// its scanner, caching in `cache`.
fn catalog(extra: Vec<PathBuf>, cache: PathBuf) -> PluginCatalog {
    let (_, host) = binaries();
    let scanner = HostLauncher::new(host, Vec::<String>::new()).scanner();
    let mut catalog = PluginCatalog::new(scanner)
        .with_cache_file(cache)
        .with_folders(PluginFormatKind::Clap, vec![])
        .with_folders(PluginFormatKind::Lv2, vec![])
        .with_folders(PluginFormatKind::Vst3, vec![]);
    catalog.set_extra_folders(extra);
    catalog
}

/// Polls `catalog` as the app's timer would until its scan ends.
fn finish_scan(catalog: &mut PluginCatalog) {
    let start = Instant::now();
    while catalog.is_scanning() {
        catalog.poll();
        assert!(
            start.elapsed() < Duration::from_secs(60),
            "the scan did not finish"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn config() -> SessionConfig {
    SessionConfig {
        engine: EngineConfig {
            sample_rate: RATE,
            max_frames: BLOCK as u32,
            ..Default::default()
        },
        inputs: 0,
        outputs: 2,
    }
}

fn gain_plugin() -> Device {
    Device::new(
        "Gain",
        DeviceSource::Plugin {
            format: PluginFormatKind::Clap,
            id: ID.into(),
        },
    )
}

/// The test plugin's state for `gain`.
fn gain_state(gain: f64) -> Vec<u8> {
    let mut s = b"DAWT".to_vec();
    s.extend_from_slice(&gain.to_le_bytes());
    s
}

struct Rig {
    doc: Document,
    session: Session,
    proc: EngineProcessor,
    track: TrackId,
    device: DeviceId,
    dir: tempfile::TempDir,
    /// What ticks reported.
    news: Vec<Warning>,
    /// Devices ticks reported a state change of.
    changed: Vec<DeviceId>,
}

impl Rig {
    /// An audio track playing a constant `LEVEL` through the test plugin.
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dc.wav");
        let data = AudioData {
            sample_rate: RATE as u32,
            channels: vec![vec![LEVEL; 10 * RATE as usize]],
        };
        write_wav(&path, &data, WavFormat::Float32).unwrap();

        let mut doc = Document::new(Project::new("test"));
        let track = Track::new(TrackKind::Audio, "Audio 1");
        let id = track.id;
        let clip = Clip::new(
            "dc",
            TickTime(0),
            TickTime(16 * PPQ),
            ClipContent::Audio(AudioClip::new(path, SampleTime(0))),
        );
        let device = gain_plugin();
        let device_id = device.id;
        doc.execute(daw_model::commands::AddTrack::new(0, track))
            .unwrap();
        doc.execute(AddClip::new(id, clip)).unwrap();
        doc.execute(AddDevice {
            track: id,
            index: 0,
            device,
        })
        .unwrap();
        Self::with_doc(doc, id, device_id, dir)
    }

    fn with_doc(doc: Document, track: TrackId, device: DeviceId, dir: tempfile::TempDir) -> Self {
        let (mut session, proc) = Session::new(config());
        session.media_mut().set_memory_limit(60.0);
        session.set_plugin_loader(Some(Box::new(loader())));
        let mut rig = Self {
            doc,
            session,
            proc,
            track,
            device,
            dir,
            news: Vec::new(),
            changed: Vec::new(),
        };
        rig.sync();
        rig.session.play().unwrap();
        rig
    }

    fn sync(&mut self) {
        let snap = self.doc.snapshot();
        self.session.sync(&snap).unwrap();
    }

    /// Runs `frames` samples, ticking the session as the app's timer would, and returns the
    /// left channel.
    fn run(&mut self, frames: usize) -> Vec<f32> {
        let mut out = Vec::with_capacity(frames);
        let mut l = vec![0.0; BLOCK];
        let mut r = vec![0.0; BLOCK];
        let events = EventList::with_capacity(0);
        while out.len() < frames {
            let n = BLOCK.min(frames - out.len());
            let mut outs: [&mut [f32]; 2] = [&mut l[..n], &mut r[..n]];
            let before = violation_count();
            assert_no_alloc(|| self.proc.process(n, &[], &mut outs, &events));
            assert_eq!(violation_count(), before, "process allocated");
            out.extend_from_slice(&l[..n]);
            let report = self.session.tick();
            assert!(report.error.is_none(), "{:?}", report.error);
            self.news.extend(report.warnings);
            self.changed.extend(report.state_changed);
        }
        out
    }

    /// Runs until a tick reports a state change of the device.
    fn reports_change(&mut self) {
        let start = Instant::now();
        self.changed.clear();
        while !self.changed.contains(&self.device) {
            assert!(
                start.elapsed() < Duration::from_secs(20),
                "no change reported"
            );
            self.run(BLOCK);
        }
        self.changed.clear();
    }

    /// Records the plugin's state as an undo step, as the app does when a change is reported.
    fn record_state(&mut self) {
        let state = self
            .session
            .plugin_state(self.device)
            .expect("a changed state");
        self.doc.seal();
        self.doc
            .execute(SetDevice {
                id: self.device,
                field: DeviceField::State(Some(state.unwrap())),
            })
            .unwrap();
        self.doc.seal();
        self.sync();
    }

    /// Runs until the output settles at `level` (the plugin and graph swaps take a few
    /// blocks), failing after a few seconds of audio.
    fn settles_at(&mut self, level: f32) {
        let start = Instant::now();
        loop {
            let out = self.run(BLOCK * 4);
            let tail = &out[out.len() - BLOCK..];
            if tail.iter().all(|&s| (s - level).abs() < 1e-5) {
                return;
            }
            assert!(
                start.elapsed() < Duration::from_secs(20),
                "output stayed at {} instead of {level}",
                tail[BLOCK - 1]
            );
        }
    }

    fn status(&self) -> Option<PluginStatus> {
        self.session.plugin_status(self.device)
    }

    /// Stores the plugins' states in the document, as the app does before saving.
    fn store_states(&mut self) -> usize {
        let states = self.session.plugin_states();
        let n = states.len();
        for s in states {
            self.doc
                .store_plugin_state(s.device, Some(s.state.unwrap()))
                .unwrap();
        }
        if n > 0 {
            self.sync();
        }
        n
    }
}

#[test]
fn a_plugin_device_loads_plays_and_takes_params() {
    let mut rig = Rig::new();
    assert_eq!(rig.status(), Some(PluginStatus::Running));
    assert!(
        rig.session.warnings().is_empty(),
        "{:?}",
        rig.session.warnings()
    );
    let params = rig.session.plugin_params(rig.device).unwrap();
    assert!(params.iter().any(|p| p.id == GAIN && p.name == "Gain"));
    rig.settles_at(LEVEL);

    let swaps = rig.session.controller().graphs_swapped();
    rig.doc
        .execute(SetParam {
            device: rig.device,
            param: GAIN,
            value: Some(0.5),
        })
        .unwrap();
    rig.sync();
    rig.settles_at(LEVEL * 0.5);
    assert_eq!(
        rig.session.controller().graphs_swapped(),
        swaps,
        "a param edit doesn't rebuild"
    );
    let value = rig.session.with_plugin(rig.device, |p| p.param_value(GAIN));
    assert_eq!(value, Some(0.5));
}

#[test]
fn plugin_params_follow_automation() {
    let mut rig = Rig::new();
    let mut lane = AutomationLane::new(AutomationTarget::DeviceParam {
        device: rig.device,
        param: GAIN,
    });
    lane.points.push(AutomationPoint {
        tick: TickTime(0),
        value: 0.25,
        curve: 0.0,
        step: true,
    });
    let id = lane.id;
    rig.doc
        .execute(AddLane {
            track: rig.track,
            lane,
        })
        .unwrap();
    rig.sync();
    rig.settles_at(LEVEL * 0.25);

    // Overwritten by hand for a while (touch write), then playing again: the
    // value from before the lane is still what it goes back to.
    let key = Session::device_key(rig.device);
    rig.session.set_suspended_lanes([id].into());
    rig.session.set_live_param(key, GAIN, Some(0.6)).unwrap();
    rig.sync();
    rig.settles_at(LEVEL * 0.6);
    rig.session.set_live_param(key, GAIN, None).unwrap();
    rig.session.set_suspended_lanes(Default::default());
    rig.sync();
    rig.settles_at(LEVEL * 0.25);

    // Without the lane, the plugin goes back to the value it had before.
    rig.doc
        .execute(daw_model::commands::RemoveLane::new(id))
        .unwrap();
    rig.sync();
    rig.settles_at(LEVEL);
    let value = rig.session.with_plugin(rig.device, |p| p.param_value(GAIN));
    assert_eq!(value, Some(1.0));
}

#[test]
fn plugin_state_saves_and_reloads_identically() {
    let mut rig = Rig::new();
    // Change the plugin behind the document's back, as its own editor would.
    let set = rig
        .session
        .with_plugin(rig.device, |p| p.load_state(&gain_state(0.75)));
    set.unwrap().unwrap();
    rig.settles_at(LEVEL * 0.75);
    let undo_steps = rig.doc.history().undo_len();
    assert_eq!(rig.store_states(), 1);
    assert_eq!(
        rig.doc.history().undo_len(),
        undo_steps,
        "the plugin's own change is no undo step"
    );

    assert_eq!(
        rig.store_states(),
        0,
        "stored states count as the document's"
    );
    let saved = rig.session.with_plugin(rig.device, |p| p.save_state());
    let saved = saved.unwrap().unwrap();

    let bundle = rig.dir.path().join("song");
    rig.doc.save_as(&bundle).unwrap();
    let doc = Document::open(&bundle).unwrap();
    let (track, device) = (rig.track, rig.device);
    let mut again = Rig::with_doc(doc, track, device, tempfile::tempdir().unwrap());
    drop(rig);
    assert_eq!(again.status(), Some(PluginStatus::Running));
    again.settles_at(LEVEL * 0.75);
    let reloaded = again.session.with_plugin(device, |p| p.save_state());
    assert_eq!(reloaded.unwrap().unwrap(), saved);
    assert_eq!(again.store_states(), 0);

    // Undoing the stored state puts the old one back into the plugin.
    let mut rig = again;
    rig.doc
        .execute(SetDevice {
            id: device,
            field: DeviceField::State(Some(gain_state(0.25).into())),
        })
        .unwrap();
    rig.sync();
    rig.settles_at(LEVEL * 0.25);
    rig.doc.undo().unwrap();
    rig.sync();
    rig.settles_at(LEVEL * 0.75);
}

#[test]
fn plugins_carry_over_to_a_new_session() {
    let mut rig = Rig::new();
    let set = rig
        .session
        .with_plugin(rig.device, |p| p.load_state(&gain_state(1.5)));
    set.unwrap().unwrap();
    rig.settles_at(LEVEL * 1.5);
    // What the audio host does when audio restarts.
    let plugins = rig.session.take_plugins();
    let (mut session, proc) = Session::new(config());
    session.media_mut().set_memory_limit(60.0);
    session.set_plugins(plugins);
    rig.session = session;
    rig.proc = proc;
    rig.sync();
    rig.session.play().unwrap();
    assert_eq!(rig.status(), Some(PluginStatus::Running));
    rig.settles_at(LEVEL * 1.5);
}

#[test]
fn a_crashed_plugin_is_reported_bypassed_and_reloaded() {
    let mut rig = Rig::new();
    rig.doc
        .execute(SetParam {
            device: rig.device,
            param: GAIN,
            value: Some(0.5),
        })
        .unwrap();
    rig.sync();
    rig.settles_at(LEVEL * 0.5);

    let monitor = rig.session.plugin_monitor(rig.device).unwrap();
    // SAFETY: plain syscall on a process this test started.
    assert_eq!(
        unsafe { libc::kill(monitor.pid() as libc::pid_t, libc::SIGKILL) },
        0
    );
    let start = Instant::now();
    while rig.status() != Some(PluginStatus::Crashed) {
        assert!(start.elapsed() < Duration::from_secs(20), "crash unnoticed");
        rig.run(BLOCK);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        rig.news
            .iter()
            .any(|w| matches!(w, Warning::PluginCrashed { device, plugin } if device == "Gain" && plugin == ID)),
        "{:?}",
        rig.news
    );
    // Bypassed: the signal goes through untouched.
    rig.settles_at(LEVEL);
    assert_eq!(rig.session.with_plugin(rig.device, |_| ()), None);

    assert!(rig.session.reload_plugin(rig.device));
    let start = Instant::now();
    while rig.status() != Some(PluginStatus::Running) {
        assert!(start.elapsed() < Duration::from_secs(20), "no reload");
        rig.run(BLOCK);
    }
    rig.settles_at(LEVEL * 0.5);
}

#[test]
fn a_missing_plugin_is_reported_and_bypassed() {
    let mut rig = Rig::new();
    let mut missing = gain_plugin();
    missing.name = "Lost".into();
    missing.source = DeviceSource::Plugin {
        format: PluginFormatKind::Clap,
        id: "org.example.nowhere".into(),
    };
    let lost = missing.id;
    rig.doc
        .execute(AddDevice {
            track: rig.track,
            index: 1,
            device: missing,
        })
        .unwrap();
    rig.sync();
    assert_eq!(
        rig.session.warnings(),
        &[Warning::PluginMissing {
            device: "Lost".into(),
            plugin: "org.example.nowhere".into()
        }]
    );
    assert_eq!(rig.session.plugin_status(lost), Some(PluginStatus::Missing));
    // The gain before it still plays; the missing one passes its signal on.
    rig.settles_at(LEVEL);
    // Not tried again on every sync.
    rig.doc
        .execute(SetParam {
            device: rig.device,
            param: GAIN,
            value: Some(0.5),
        })
        .unwrap();
    rig.sync();
    assert!(rig.session.warnings().is_empty());
    rig.settles_at(LEVEL * 0.5);
}

#[test]
fn without_a_loader_plugins_are_bypassed() {
    let mut rig = Rig::new();
    rig.session.set_plugin_loader(None);
    // Loaded plugins stay; a new device can't load.
    let mut other = gain_plugin();
    other.name = "Other".into();
    let other_id = other.id;
    rig.doc
        .execute(AddDevice {
            track: rig.track,
            index: 1,
            device: other,
        })
        .unwrap();
    rig.sync();
    assert_eq!(
        rig.session.plugin_status(other_id),
        Some(PluginStatus::NoLoader)
    );
    assert!(matches!(
        rig.session.warnings(),
        [Warning::PluginNotLoaded { .. }]
    ));
    rig.settles_at(LEVEL);
}

#[test]
fn the_catalog_scans_added_folders_in_the_background() {
    let (plugins, _) = binaries();
    let dir = tempfile::tempdir().unwrap();
    let cache = dir.path().join("cache").join("plugins.json");
    let mut catalog = catalog(vec![plugins.clone()], cache.clone());
    assert!(!catalog.has_scanned() && catalog.progress().is_none());

    let revision = catalog.revision();
    catalog.scan(false);
    assert!(catalog.is_scanning() && catalog.progress().is_some());
    // Asked again while it runs: queued, not started twice.
    catalog.scan(false);
    finish_scan(&mut catalog);
    assert!(catalog.has_scanned());
    assert!(catalog.revision() > revision);
    assert!(catalog.failed().is_empty(), "{:?}", catalog.failed());
    let found = catalog
        .find(&PluginFormatKind::Clap, ID)
        .expect("the test plugin is listed");
    assert!(!found.is_instrument);
    assert!(found.path.starts_with(plugins));
    assert!(cache.is_file(), "results are cached");

    let effects = Filter {
        text: "gain".into(),
        kind: Some(PluginKind::Effect),
        ..Default::default()
    };
    assert_eq!(catalog.filtered(&effects).count(), 1);
    let instruments = Filter {
        kind: Some(PluginKind::Instrument),
        ..Default::default()
    };
    assert_eq!(catalog.filtered(&instruments).count(), 0);

    // The same plugins again: the list counts as unchanged.
    let list = catalog.list_revision();
    catalog.scan(false);
    finish_scan(&mut catalog);
    assert_eq!(catalog.list_revision(), list);

    // Without the folder the plugin is gone, even though the cache still has it.
    catalog.set_extra_folders(vec![]);
    catalog.scan(true);
    finish_scan(&mut catalog);
    assert!(catalog.plugins().is_empty());
    assert_ne!(catalog.list_revision(), list);
}

#[test]
fn a_missing_plugin_loads_once_a_scan_finds_it() {
    let mut rig = Rig::new();
    // A loader that finds nothing by itself, as when the plugin is in a folder the user added.
    let mut blind = loader();
    for format in daw_session::catalog::FORMATS {
        blind.registry_mut().set_search_paths(format, vec![]);
    }
    rig.session.set_plugin_loader(Some(Box::new(blind)));
    let mut second = gain_plugin();
    second.name = "Second".into();
    let second_id = second.id;
    rig.doc
        .execute(AddDevice {
            track: rig.track,
            index: 1,
            device: second,
        })
        .unwrap();
    rig.sync();
    assert_eq!(
        rig.session.plugin_status(second_id),
        Some(PluginStatus::Missing)
    );

    let (plugins, _) = binaries();
    let dir = tempfile::tempdir().unwrap();
    let mut catalog = catalog(vec![plugins.clone()], dir.path().join("plugins.json"));
    catalog.scan(false);
    finish_scan(&mut catalog);
    rig.session.set_known_plugins(catalog.plugins());
    rig.sync();
    assert_eq!(
        rig.session.plugin_status(second_id),
        Some(PluginStatus::Running)
    );
    // Two gains of 1 in a row.
    rig.settles_at(LEVEL);
}

#[test]
fn document_params_apply_on_top_of_a_loaded_state() {
    let mut rig = Rig::new();
    rig.doc
        .execute(SetParam {
            device: rig.device,
            param: GAIN,
            value: Some(1.5),
        })
        .unwrap();
    rig.sync();
    rig.settles_at(LEVEL * 1.5);
    // A state edit sets the plugin's gain, then the document's param goes on top again,
    // as when the project loads.
    rig.doc
        .execute(SetDevice {
            id: rig.device,
            field: DeviceField::State(Some(gain_state(0.25).into())),
        })
        .unwrap();
    rig.sync();
    rig.settles_at(LEVEL * 1.5);
    rig.doc
        .execute(SetParam {
            device: rig.device,
            param: GAIN,
            value: Some(0.5),
        })
        .unwrap();
    rig.sync();
    rig.settles_at(LEVEL * 0.5);
}

#[test]
fn undoing_a_device_removal_brings_its_plugin_back_as_it_was() {
    let mut rig = Rig::new();
    let set = rig
        .session
        .with_plugin(rig.device, |p| p.load_state(&gain_state(0.75)));
    set.unwrap().unwrap();
    rig.settles_at(LEVEL * 0.75);
    rig.doc.execute(RemoveDevice::new(rig.device)).unwrap();
    rig.sync();
    rig.settles_at(LEVEL);
    assert_eq!(rig.status(), None);
    rig.doc.undo().unwrap();
    rig.sync();
    rig.settles_at(LEVEL * 0.75);
}

#[test]
fn an_editor_change_becomes_an_undo_step() {
    let mut rig = Rig::new();
    assert!(rig.session.plugin_has_editor(rig.device));
    assert!(!rig.session.plugin_editor_open(rig.device));
    rig.session
        .open_plugin_editor(rig.device, "Gain — Audio 1")
        .expect("open the editor");
    assert!(rig.session.plugin_editor_open(rig.device));
    // Opening an open editor again changes nothing.
    rig.session
        .open_plugin_editor(rig.device, "Gain — Audio 1")
        .unwrap();
    // The test plugin says its state changed when its editor shows.
    rig.reports_change();
    rig.record_state();
    assert_eq!(
        rig.session.plugin_state(rig.device).map(|s| s.is_ok()),
        None
    );

    // The user turns the gain down in the editor. (A load is no report of the plugin's own;
    // showing the test plugin's editor again is.)
    let set = rig
        .session
        .with_plugin(rig.device, |p| p.load_state(&gain_state(0.5)));
    set.unwrap().unwrap();
    rig.settles_at(LEVEL * 0.5);
    rig.session.open_plugin_editor(rig.device, "Gain").unwrap();
    rig.reports_change();
    let steps = rig.doc.history().undo_len();
    rig.record_state();
    assert_eq!(rig.doc.history().undo_len(), steps + 1);
    // The plugin is not handed its own state back.
    rig.settles_at(LEVEL * 0.5);
    assert_eq!(
        rig.session.plugin_state(rig.device).map(|s| s.is_ok()),
        None
    );

    // Undo puts the plugin back to where the editor started.
    rig.doc.undo().unwrap();
    rig.sync();
    rig.settles_at(LEVEL);
    rig.doc.redo().unwrap();
    rig.sync();
    rig.settles_at(LEVEL * 0.5);
}

#[test]
fn closing_the_editor_is_no_change() {
    let mut rig = Rig::new();
    rig.session.open_plugin_editor(rig.device, "Gain").unwrap();
    rig.reports_change();
    // The host sets a parameter while the editor is open, then the user closes it.
    rig.doc
        .execute(SetParam {
            device: rig.device,
            param: GAIN,
            value: Some(0.5),
        })
        .unwrap();
    rig.sync();
    rig.settles_at(LEVEL * 0.5);
    let closed = rig
        .session
        .with_plugin(rig.device, |p| p.load_state(b"DAWC"));
    closed.unwrap().unwrap();
    // The sandbox host notices at its own pace.
    let start = Instant::now();
    while rig.session.plugin_editor_open(rig.device) {
        assert!(start.elapsed() < Duration::from_secs(20), "close unnoticed");
        rig.run(BLOCK);
        std::thread::sleep(Duration::from_millis(5));
    }
    rig.run(BLOCK * 8);
    assert!(rig.changed.is_empty(), "{:?}", rig.changed);
}

#[test]
fn an_open_editor_opens_again_after_audio_restarts() {
    let mut rig = Rig::new();
    rig.session
        .open_plugin_editor(rig.device, "Gain — Audio 1")
        .unwrap();
    let plugins = rig.session.take_plugins();
    let (mut session, proc) = Session::new(config());
    session.media_mut().set_memory_limit(60.0);
    session.set_plugins(plugins);
    rig.session = session;
    rig.proc = proc;
    rig.sync();
    rig.session.play().unwrap();
    assert_eq!(rig.status(), Some(PluginStatus::Running));
    assert!(rig.session.plugin_editor_open(rig.device));
}

#[test]
fn removing_the_device_closes_its_editor() {
    let mut rig = Rig::new();
    rig.session.open_plugin_editor(rig.device, "Gain").unwrap();
    rig.doc.execute(RemoveDevice::new(rig.device)).unwrap();
    rig.sync();
    assert!(!rig.session.plugin_editor_open(rig.device));
    assert!(!rig.session.plugin_has_editor(rig.device));
    // Undo brings the device back with its editor closed.
    rig.doc.undo().unwrap();
    rig.sync();
    rig.settles_at(LEVEL);
    assert!(!rig.session.plugin_editor_open(rig.device));
    assert!(rig.session.open_plugin_editor(rig.device, "Gain").is_ok());
}

#[test]
fn a_crashed_plugin_has_no_editor_to_open() {
    let mut rig = Rig::new();
    rig.session.open_plugin_editor(rig.device, "Gain").unwrap();
    let pid = rig.session.plugin_monitor(rig.device).unwrap().pid();
    // SAFETY: plain syscall on a process this test started.
    assert_eq!(unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) }, 0);
    let start = Instant::now();
    while rig.status() != Some(PluginStatus::Crashed) {
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "crash not noticed"
        );
        rig.run(BLOCK);
    }
    assert!(!rig.session.plugin_editor_open(rig.device));
    assert!(rig.session.open_plugin_editor(rig.device, "Gain").is_err());
    // A reload opens the editor again.
    assert!(rig.session.reload_plugin(rig.device));
    while rig.status() != Some(PluginStatus::Running) {
        assert!(start.elapsed() < Duration::from_secs(20), "not reloaded");
        rig.run(BLOCK);
    }
    assert!(rig.session.plugin_editor_open(rig.device));
}

/// Renders bar 1 of `rig`'s project offline, loading plugins with `loader`.
fn export_bar(
    rig: &mut Rig,
    loader: Option<Box<dyn daw_session::PluginLoader>>,
) -> (Vec<f32>, Vec<Warning>) {
    let mut out = Vec::new();
    let warnings = try_export(rig, loader, 4 * PPQ, |planes| {
        out.extend_from_slice(planes[0]);
    })
    .unwrap();
    (out, warnings)
}

fn try_export(
    rig: &mut Rig,
    loader: Option<Box<dyn daw_session::PluginLoader>>,
    ticks: i64,
    mut sink: impl FnMut(&[&[f32]]),
) -> Result<Vec<Warning>, daw_session::export::ExportError> {
    use daw_session::export::{Progress, RenderSettings, render};
    render(
        &rig.doc.snapshot(),
        Some(rig.dir.path().to_path_buf()),
        loader,
        &RenderSettings {
            start: TickTime(0),
            end: TickTime(ticks),
            tail_seconds: 0.0,
            sample_rate: RATE as u32,
        },
        &Progress::default(),
        |planes| {
            sink(planes);
            Ok(())
        },
    )
}

/// Loads through a [`SandboxLoader`], keeping each sandbox host's pid.
struct PidLoader(SandboxLoader, std::sync::Arc<std::sync::Mutex<Vec<u32>>>);

impl daw_session::PluginLoader for PidLoader {
    fn load(
        &mut self,
        format: &PluginFormatKind,
        id: &str,
        sample_rate: f64,
        max_frames: u32,
    ) -> Result<daw_session::LoadedPlugin, daw_session::LoadError> {
        let loaded = self.0.load(format, id, sample_rate, max_frames)?;
        if let Some(m) = &loaded.monitor {
            self.1.lock().unwrap().push(m.pid());
        }
        Ok(loaded)
    }
}

#[test]
fn exports_play_plugins_with_their_stored_state() {
    let mut rig = Rig::new();
    rig.doc
        .store_plugin_state(rig.device, Some(gain_state(0.5).into()))
        .unwrap();

    let (out, warnings) = export_bar(&mut rig, Some(Box::new(export_loader())));
    assert!(warnings.is_empty(), "{warnings:?}");
    // Two seconds at 120 bpm through the plugin at its stored gain. The plugin
    // reports 32 samples of latency but doesn't delay, so the export, which
    // compensates for it, starts 32 samples into the clip's 64-sample fade-in
    // and ends with 32 samples of silence.
    let latency = 32; // daw_clap_test_plugin::LATENCY
    assert_eq!(out.len(), 2 * RATE as usize);
    let body = &out[latency..out.len() - latency];
    let worst = body
        .iter()
        .fold(0.0f32, |m, s| m.max((s - LEVEL * 0.5).abs()));
    assert!(
        worst < 1e-5,
        "export strays {worst} from the plugin's output"
    );
    assert!(out[out.len() - latency..].iter().all(|&s| s == 0.0));

    // Without a loader the device is bypassed, and the export says so.
    let (out, warnings) = export_bar(&mut rig, None);
    assert!(
        matches!(warnings.as_slice(), [Warning::PluginNotLoaded { .. }]),
        "{warnings:?}"
    );
    // Past the clip's fade-in, the clip as it is.
    assert!(out[64..].iter().all(|s| (s - LEVEL).abs() < 1e-5));
}

#[test]
fn a_plugin_crashing_mid_export_fails_it() {
    use daw_session::export::ExportError;
    let mut rig = Rig::new();
    let pids = std::sync::Arc::default();
    let loader = PidLoader(export_loader(), std::sync::Arc::clone(&pids));
    let mut blocks = 0;
    let result = try_export(&mut rig, Some(Box::new(loader)), 16 * PPQ, |_| {
        blocks += 1;
        if blocks == 1 {
            let pid = pids.lock().unwrap()[0];
            // SAFETY: plain syscall on a process this test started.
            assert_eq!(unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) }, 0);
            std::thread::sleep(Duration::from_millis(200));
        }
    });
    // Never a file quietly missing the plugin.
    assert!(
        matches!(
            result,
            Err(ExportError::PluginCrashed(ref d)) if d == "Gain"
        ) || matches!(result, Err(ExportError::PluginTooSlow(_))),
        "{result:?}"
    );
}

/// A step lane holding the test plugin's gain at `value`, added to the rig.
fn gain_lane(rig: &mut Rig, value: f64) -> daw_model::LaneId {
    let mut lane = AutomationLane::new(AutomationTarget::DeviceParam {
        device: rig.device,
        param: GAIN,
    });
    lane.points.push(AutomationPoint {
        tick: TickTime(0),
        value,
        curve: 0.0,
        step: true,
    });
    let id = lane.id;
    rig.doc
        .execute(AddLane {
            track: rig.track,
            lane,
        })
        .unwrap();
    rig.sync();
    id
}

fn remove_lane(rig: &mut Rig, id: daw_model::LaneId) {
    rig.doc
        .execute(daw_model::commands::RemoveLane::new(id))
        .unwrap();
    rig.sync();
}

#[test]
fn a_removed_lane_goes_back_after_a_save_moves_the_project() {
    let mut rig = Rig::new();
    let id = gain_lane(&mut rig, 0.25);
    rig.settles_at(LEVEL * 0.25);
    // Saving a new project moves its root, which rebuilds everything.
    let saved = rig.dir.path().join("saved");
    std::fs::create_dir_all(&saved).unwrap();
    rig.session.set_root(Some(saved));
    rig.sync();
    rig.settles_at(LEVEL * 0.25);
    remove_lane(&mut rig, id);
    rig.settles_at(LEVEL);
}

#[test]
fn a_lane_written_by_hand_goes_back_to_the_value_before_the_take() {
    let mut rig = Rig::new();
    // A take with no lane yet holds the gain while the knob moves...
    let key = Session::device_key(rig.device);
    rig.session.set_live_param(key, GAIN, Some(0.4)).unwrap();
    rig.session.set_live_param(key, GAIN, Some(0.6)).unwrap();
    rig.settles_at(LEVEL * 0.6);
    // ...and ends as a lane in the document.
    rig.session.set_live_param(key, GAIN, None).unwrap();
    let id = gain_lane(&mut rig, 0.25);
    rig.settles_at(LEVEL * 0.25);
    remove_lane(&mut rig, id);
    rig.settles_at(LEVEL);
}

#[test]
fn a_state_loaded_under_a_lane_is_what_removing_it_goes_back_to() {
    let mut rig = Rig::new();
    let id = gain_lane(&mut rig, 0.25);
    rig.settles_at(LEVEL * 0.25);
    // A preset loaded while the lane plays (or an undone state change).
    rig.doc
        .execute(SetDevice {
            id: rig.device,
            field: DeviceField::State(Some(gain_state(0.75).into())),
        })
        .unwrap();
    rig.sync();
    rig.settles_at(LEVEL * 0.25);
    remove_lane(&mut rig, id);
    rig.settles_at(LEVEL * 0.75);
}
