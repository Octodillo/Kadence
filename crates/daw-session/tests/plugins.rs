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

/// A loader that finds only the test plugin.
fn loader() -> SandboxLoader {
    let (plugins, host) = binaries();
    let mut config = SandboxConfig::new(HostLauncher::new(host, Vec::<String>::new()));
    // CI machines are slow and the host is a debug build.
    config.process_timeout = Some(Duration::from_secs(5));
    config.rt_priority = None;
    let mut loader = SandboxLoader::new(config);
    let registry = loader.registry_mut();
    registry.set_search_paths(PluginFormatKind::Clap, vec![plugins.clone()]);
    registry.set_search_paths(PluginFormatKind::Lv2, vec![]);
    registry.set_search_paths(PluginFormatKind::Vst3, vec![]);
    loader
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
    rig.doc
        .execute(AddLane {
            track: rig.track,
            lane,
        })
        .unwrap();
    rig.sync();
    rig.settles_at(LEVEL * 0.25);
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
