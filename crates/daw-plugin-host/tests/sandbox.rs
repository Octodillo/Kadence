//! Runs the `daw-clap-test-plugin` CLAP in a sandbox host process and checks that it processes
//! like an in-process plugin, and that killing or freezing the host leaves us running and able
//! to reload it.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use assert_no_alloc::{AllocDisabler, assert_no_alloc};
use daw_core::event::{Event, EventKind, EventList};
use daw_core::node::{Node, ProcessContext, TransportState};
use daw_core::plugin::{
    PluginDescriptor, PluginError, PluginFormat, PluginFormatKind, PluginInstance,
};
use daw_plugin_host::{
    CrashReason, HostLauncher, SandboxConfig, SandboxFormat, SandboxStatus, SandboxedInstance,
};

#[global_allocator]
static ALLOC: AllocDisabler = AllocDisabler;

const ID: &str = "org.daw.test.gain";
const GAIN: u32 = 1;
const LAST_NOTE: u32 = 2;
const SR: f64 = 48_000.0;
const MAX: u32 = 256;

/// Builds the test plugin in its own target directory and returns a `.clap` copy of it, made
/// once per test process.
fn test_plugin() -> PathBuf {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(|| {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let target = root.join("target").join("clap-test-plugin");
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let status = Command::new(cargo)
            .args([
                "build",
                "--quiet",
                "--package",
                "daw-clap-test-plugin",
                "--target-dir",
            ])
            .arg(&target)
            .current_dir(&root)
            .status()
            .expect("run cargo");
        assert!(status.success(), "building the test plugin failed");
        let lib = target.join("debug").join("libdaw_clap_test_plugin.so");
        let clap = target.join(format!("daw-sandbox-test-{}.clap", std::process::id()));
        std::fs::copy(&lib, &clap).expect("copy the test plugin");
        clap
    })
    .clone()
}

fn config() -> SandboxConfig {
    let mut config = SandboxConfig::new(HostLauncher::new(
        env!("CARGO_BIN_EXE_daw-plugin-host"),
        Vec::<String>::new(),
    ));
    // CI machines are slow and the host is a debug build: be generous per cycle, except where
    // a test is about timing.
    config.process_timeout = Some(Duration::from_secs(5));
    config.rt_priority = None;
    config
}

fn format() -> SandboxFormat {
    SandboxFormat::new(PluginFormatKind::Clap, vec![], config())
}

fn descriptor() -> PluginDescriptor {
    let mut found = format()
        .scan(&test_plugin())
        .expect("the test plugin loads");
    assert_eq!(found.len(), 1, "one plugin in the test bundle");
    found.remove(0)
}

fn instance_with(config: SandboxConfig) -> SandboxedInstance {
    SandboxedInstance::new(descriptor(), config, SR, MAX).expect("instantiate in the sandbox")
}

fn instance() -> SandboxedInstance {
    instance_with(config())
}

/// Runs one block of `frames` stereo frames with every input sample set to `level`.
fn run(
    plugin: &mut dyn PluginInstance,
    frames: usize,
    level: f32,
    events: &[Event],
) -> ([Vec<f32>; 2], Vec<Event>) {
    let input = vec![level; frames];
    let mut left = vec![f32::NAN; frames];
    let mut right = vec![f32::NAN; frames];
    let mut events_in = EventList::with_capacity(64);
    for e in events {
        events_in.push(*e).unwrap();
    }
    let mut events_out = EventList::with_capacity(64);
    let transport = TransportState {
        playing: true,
        bpm: 120.0,
        sample_rate: SR,
        ..TransportState::default()
    };
    {
        let audio_in = [&input[..], &input[..]];
        let mut audio_out = [&mut left[..], &mut right[..]];
        let mut ctx = ProcessContext {
            frames: u32::try_from(frames).unwrap(),
            transport: &transport,
            audio_in: &audio_in,
            audio_out: &mut audio_out,
            events_in: &events_in,
            events_out: &mut events_out,
        };
        plugin.process(&mut ctx);
    }
    let out = events_out.iter().copied().collect();
    ([left, right], out)
}

fn param(param: u32, value: f64, frame: u32) -> Event {
    Event {
        frame,
        kind: EventKind::ParamValue { param, value },
    }
}

fn signal(pid: u32, sig: libc::c_int) {
    // SAFETY: plain syscall on a process this test started.
    assert_eq!(unsafe { libc::kill(pid as libc::pid_t, sig) }, 0);
}

/// Whether every thread of `pid` is stopped (state `T` in `/proc`).
fn stopped(pid: u32) -> bool {
    let Ok(tasks) = std::fs::read_dir(format!("/proc/{pid}/task")) else {
        return false;
    };
    tasks.flatten().all(|t| {
        std::fs::read_to_string(t.path().join("stat")).is_ok_and(|stat| {
            stat.rsplit_once(") ")
                .is_some_and(|(_, rest)| rest.starts_with('T'))
        })
    })
}

fn wait_for(what: &str, timeout: Duration, mut done: impl FnMut() -> bool) {
    let start = Instant::now();
    while !done() {
        assert!(start.elapsed() < timeout, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn scanning_through_the_host_describes_the_plugin() {
    let d = descriptor();
    assert_eq!(d.format, PluginFormatKind::Clap);
    assert_eq!(d.id, ID);
    assert_eq!(d.ports, (2, 2));
    // A bundle that does not load says why.
    match format().scan(Path::new("/nonexistent.clap")) {
        Err(PluginError::Load(why)) => assert!(why.contains("/nonexistent.clap"), "{why}"),
        other => panic!("expected a load error, got {other:?}"),
    }
}

#[test]
fn reports_ports_params_and_latency_from_the_host() {
    let plugin = instance();
    let ports = plugin.ports();
    assert_eq!((ports.audio_in, ports.audio_out), (2, 2));
    assert!(ports.event_in && ports.event_out);
    assert_eq!(plugin.latency(), 32);
    let params = plugin.params();
    assert_eq!(params.len(), 2);
    let gain = params.iter().find(|p| p.id == GAIN).unwrap();
    assert_eq!((gain.min, gain.max, gain.default), (0.0, 2.0, 1.0));
    assert!(!plugin.has_editor());
    assert_eq!(plugin.descriptor().id, ID);
    let monitor = plugin.monitor();
    assert_eq!(monitor.status(), SandboxStatus::Running);
    assert_ne!(monitor.pid(), std::process::id());
}

#[test]
fn processes_audio_and_sample_accurate_params() {
    let mut plugin = instance();
    let ([l, r], _) = run(&mut plugin, 200, 0.5, &[param(GAIN, 0.5, 100)]);
    assert!(l[..100].iter().all(|&s| s == 0.5), "{:?}", &l[..4]);
    assert!(l[100..].iter().all(|&s| s == 0.25));
    assert_eq!(l, r);
    assert_eq!(plugin.param_value(GAIN), 0.5);
}

#[test]
fn blocks_larger_than_max_frames_are_split() {
    let mut plugin = instance();
    // Frame 600 lands in the third 256-frame chunk.
    let ([l, _], _) = run(&mut plugin, 700, 1.0, &[param(GAIN, 0.5, 600)]);
    assert!(l[..600].iter().all(|&s| s == 1.0));
    assert!(l[600..].iter().all(|&s| s == 0.5));
}

#[test]
fn a_bigger_prepare_maps_a_bigger_block() {
    let mut plugin = instance();
    plugin.prepare(SR, 2048);
    let ([l, r], _) = run(&mut plugin, 2048, 0.25, &[param(GAIN, 2.0, 1500)]);
    assert!(l[..1500].iter().all(|&s| s == 0.25));
    assert!(l[1500..].iter().all(|&s| s == 0.5));
    assert_eq!(l, r);
    assert_eq!(plugin.monitor().status(), SandboxStatus::Running);
}

#[test]
fn note_events_and_output_params_come_back() {
    let mut plugin = instance();
    let on = Event {
        frame: 70,
        kind: EventKind::NoteOn {
            channel: 1,
            key: 64,
            velocity: 0.5,
            note_id: 7,
        },
    };
    let midi = Event {
        frame: 10,
        kind: EventKind::Midi([0xb0, 1, 99]),
    };
    let (_, out) = run(&mut plugin, 128, 0.0, &[midi, on]);
    assert!(out.contains(&midi), "{out:?}");
    assert!(out.contains(&on), "{out:?}");
    assert!(out.contains(&param(LAST_NOTE, 64.0, 70)), "{out:?}");
    assert_eq!(plugin.param_value(LAST_NOTE), 64.0);
}

#[test]
fn state_round_trips_between_sandboxes() {
    let mut a = instance();
    run(&mut a, 16, 0.0, &[param(GAIN, 0.25, 0)]);
    let state = a.save_state().unwrap();

    let mut b = instance();
    assert_eq!(b.param_value(GAIN), 1.0);
    b.load_state(&state).unwrap();
    assert_eq!(b.param_value(GAIN), 0.25);
    let ([l, _], _) = run(&mut b, 16, 1.0, &[]);
    assert!(l.iter().all(|&s| s == 0.25));

    assert!(b.load_state(b"garbage").is_err());
    // A plugin error is not a crash.
    assert_eq!(b.monitor().status(), SandboxStatus::Running);
}

#[test]
fn works_through_the_registry_as_a_trait_object() {
    let mut registry = daw_plugins::PluginRegistry::new();
    registry.register(Box::new(format()));
    let mut plugin = registry.instantiate(&descriptor(), SR, MAX).unwrap();
    let ([l, _], _) = run(plugin.as_mut(), 64, 0.5, &[]);
    assert!(l.iter().all(|&s| s == 0.5));
}

#[test]
fn process_does_not_allocate() {
    let mut plugin = instance();
    let frames = 600;
    let input = vec![0.5; frames];
    let mut left = vec![0.0; frames];
    let mut right = vec![0.0; frames];
    let mut events_in = EventList::with_capacity(8);
    events_in.push(param(GAIN, 0.5, 3)).unwrap();
    events_in
        .push(Event {
            frame: 300,
            kind: EventKind::NoteOn {
                channel: 0,
                key: 60,
                velocity: 1.0,
                note_id: -1,
            },
        })
        .unwrap();
    let mut events_out = EventList::with_capacity(8);
    let transport = TransportState::default();
    for _ in 0..3 {
        events_out.clear();
        let audio_in = [&input[..], &input[..]];
        let mut audio_out = [&mut left[..], &mut right[..]];
        let mut ctx = ProcessContext {
            frames: frames as u32,
            transport: &transport,
            audio_in: &audio_in,
            audio_out: &mut audio_out,
            events_in: &events_in,
            events_out: &mut events_out,
        };
        assert_no_alloc(|| plugin.process(&mut ctx));
    }
    assert_eq!(left[0], 0.25);
    assert_eq!(left[599], 0.25);
    assert_no_alloc(|| plugin.reset());
    // The reset reaches the host on the next cycle.
    let ([l, _], _) = run(&mut plugin, 8, 1.0, &[]);
    assert!(l.iter().all(|&s| s == 0.5));
}

#[test]
fn killing_the_host_mid_playback_keeps_us_running_and_reload_restores_it() {
    let crashes = Arc::new(AtomicUsize::new(0));
    let mut config = config();
    {
        let crashes = crashes.clone();
        config.on_crash = Some(Arc::new(move |m| {
            assert!(m.is_crashed());
            // Never on the thread that drives the instance.
            assert_eq!(std::thread::current().name(), Some("plugin-watchdog"));
            crashes.fetch_add(1, Ordering::SeqCst);
        }));
    }
    let mut plugin = instance_with(config);
    let monitor = plugin.monitor();

    // A saved state, then a later change that only exists as an automation event.
    run(&mut plugin, 64, 0.0, &[param(GAIN, 0.25, 0)]);
    let state = plugin.save_state().unwrap();
    run(&mut plugin, 64, 0.0, &[param(GAIN, 0.5, 0)]);

    let old_pid = monitor.pid();
    signal(old_pid, libc::SIGKILL);
    wait_for("the crash to be noticed", Duration::from_secs(5), || {
        monitor.is_crashed()
    });
    assert!(matches!(
        monitor.status(),
        SandboxStatus::Crashed(CrashReason::Exited(_))
    ));
    assert_eq!(crashes.load(Ordering::SeqCst), 1);

    // Playback carries on, in silence, without waiting on the dead host.
    let start = Instant::now();
    for _ in 0..100 {
        let ([l, r], _) = run(&mut plugin, 256, 1.0, &[]);
        assert!(l.iter().chain(&r).all(|&s| s == 0.0));
    }
    assert!(start.elapsed() < Duration::from_secs(1));
    // Main-thread calls fail cleanly; saving keeps the last state.
    assert_eq!(plugin.save_state().unwrap(), state);
    assert!(plugin.open_editor().is_err());

    monitor.request_reload();
    plugin.on_main_thread();
    assert_eq!(monitor.status(), SandboxStatus::Running);
    assert_ne!(monitor.pid(), old_pid);
    assert_eq!(monitor.generation(), 2);
    assert_eq!(plugin.latency(), 32);

    // The saved state comes back, then the change made after it.
    let ([l, _], _) = run(&mut plugin, 32, 1.0, &[]);
    assert!(l.iter().all(|&s| s == 0.5), "{:?}", &l[..4]);
    assert_eq!(plugin.param_value(GAIN), 0.5);
    assert_eq!(crashes.load(Ordering::SeqCst), 1);
}

#[test]
fn a_frozen_host_costs_one_late_cycle_then_is_killed_as_hung() {
    let mut config = config();
    config.process_timeout = None; // one buffer's duration
    config.hang_timeout = Duration::from_millis(300);
    let mut plugin = instance_with(config);
    let monitor = plugin.monitor();
    // Warm up with the generous timeout path of a normal cycle.
    let ([l, _], _) = run(&mut plugin, 256, 1.0, &[]);
    assert!(l.iter().all(|&s| s == 1.0 || s == 0.0));

    signal(monitor.pid(), libc::SIGSTOP);
    // The stop is asynchronous; wait until every thread of the host is stopped.
    wait_for("the host to stop", Duration::from_secs(5), || {
        stopped(monitor.pid())
    });
    let start = Instant::now();
    let ([l, _], _) = run(&mut plugin, 256, 1.0, &[]);
    // 256 frames at 48 kHz is 5.3 ms; allow for scheduling.
    assert!(start.elapsed() < Duration::from_millis(100));
    assert!(l.iter().all(|&s| s == 0.0));
    assert!(monitor.late_cycles() >= 1);
    // While the host is still stuck on that cycle, later ones return at once.
    let start = Instant::now();
    run(&mut plugin, 256, 1.0, &[]);
    assert!(start.elapsed() < Duration::from_millis(50));

    wait_for("the hang to be noticed", Duration::from_secs(5), || {
        monitor.is_crashed()
    });
    assert_eq!(monitor.status(), SandboxStatus::Crashed(CrashReason::Hung));

    plugin.reload().unwrap();
    assert_eq!(monitor.status(), SandboxStatus::Running);
    let mut ok = false;
    for _ in 0..20 {
        let ([l, _], _) = run(&mut plugin, 256, 1.0, &[]);
        if l.iter().all(|&s| s == 1.0) {
            ok = true;
            break;
        }
    }
    assert!(ok, "the reloaded host processes again");
}

#[test]
fn a_missing_host_program_is_a_load_error() {
    let mut config = config();
    config.launcher = HostLauncher::new("/nonexistent/daw-plugin-host", Vec::<String>::new());
    let result = SandboxedInstance::new(descriptor(), config, SR, MAX);
    assert!(matches!(
        result,
        Err(daw_core::plugin::PluginError::Load(_))
    ));
}

#[test]
fn a_plugin_that_fails_to_load_is_a_load_error_not_a_hang() {
    let mut d = descriptor();
    d.id = "org.daw.missing".into();
    let start = Instant::now();
    assert!(SandboxedInstance::new(d, config(), SR, MAX).is_err());
    assert!(start.elapsed() < Duration::from_secs(10));
}

#[test]
fn dropping_the_instance_stops_the_host() {
    let plugin = instance();
    let pid = plugin.monitor().pid();
    drop(plugin);
    // SAFETY: signal 0 only checks whether the process exists.
    let alive = unsafe { libc::kill(pid as libc::pid_t, 0) } == 0;
    assert!(!alive, "host {pid} still running");
}

#[test]
fn events_sent_while_the_host_is_busy_arrive_late_not_never() {
    let mut config = config();
    config.process_timeout = Some(Duration::from_millis(20));
    let mut plugin = instance_with(config);
    let monitor = plugin.monitor();
    run(&mut plugin, 64, 0.0, &[]);

    signal(monitor.pid(), libc::SIGSTOP);
    wait_for("the host to stop", Duration::from_secs(5), || {
        stopped(monitor.pid())
    });
    let off = Event {
        frame: 5,
        kind: EventKind::NoteOff {
            channel: 0,
            key: Some(60),
            velocity: 0.0,
            note_id: 3,
        },
    };
    let on = Event {
        frame: 9,
        kind: EventKind::NoteOn {
            channel: 0,
            key: 62,
            velocity: 1.0,
            note_id: 4,
        },
    };
    // The first cycle times out with the note-off posted; the second finds the host still
    // busy and carries the note-on.
    run(&mut plugin, 64, 0.0, &[off]);
    run(&mut plugin, 64, 0.0, &[on]);
    assert_eq!(monitor.late_cycles(), 2);
    signal(monitor.pid(), libc::SIGCONT);

    let mut seen = Vec::new();
    for _ in 0..50 {
        let (_, out) = run(&mut plugin, 64, 0.0, &[]);
        seen.extend(out);
        if seen.len() >= 2 {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let keys: Vec<_> = seen
        .iter()
        .filter_map(|e| match e.kind {
            EventKind::NoteOff { key: Some(key), .. } | EventKind::NoteOn { key, .. } => Some(key),
            _ => None,
        })
        .collect();
    assert!(keys.contains(&60), "the note-off came out: {seen:?}");
    assert!(keys.contains(&62), "the note-on came out: {seen:?}");
    assert_eq!(monitor.status(), SandboxStatus::Running);
}

#[test]
fn saving_right_after_a_reload_still_restores_the_changes_made_since_the_last_save() {
    let mut plugin = instance();
    run(&mut plugin, 16, 0.0, &[param(GAIN, 0.25, 0)]);
    plugin.save_state().unwrap();
    run(&mut plugin, 16, 0.0, &[param(GAIN, 0.5, 0)]);

    plugin.reload().unwrap();
    // Before any cycle has re-sent the change, so the plugin's state does not have it yet.
    plugin.save_state().unwrap();
    let ([l, _], _) = run(&mut plugin, 16, 1.0, &[]);
    assert!(l.iter().all(|&s| s == 0.5), "{:?}", &l[..4]);
    // The change is still not in a saved state, so another reload brings it back too.
    plugin.reload().unwrap();
    let ([l, _], _) = run(&mut plugin, 16, 1.0, &[]);
    assert!(l.iter().all(|&s| s == 0.5), "{:?}", &l[..4]);
}

#[test]
fn a_state_load_the_host_dies_in_is_kept_for_the_reload() {
    let mut a = instance();
    run(&mut a, 16, 0.0, &[param(GAIN, 0.25, 0)]);
    let state = a.save_state().unwrap();

    let mut b = instance();
    let monitor = b.monitor();
    let pid = monitor.pid();
    signal(pid, libc::SIGSTOP);
    wait_for("the host to stop", Duration::from_secs(5), || stopped(pid));
    // The host dies while the load is in flight.
    let killer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        signal(pid, libc::SIGKILL);
    });
    let _ = b.load_state(&state);
    killer.join().unwrap();
    wait_for("the crash to be noticed", Duration::from_secs(5), || {
        monitor.is_crashed()
    });
    assert_eq!(b.save_state().unwrap(), state);

    b.reload().unwrap();
    let ([l, _], _) = run(&mut b, 16, 1.0, &[]);
    assert!(l.iter().all(|&s| s == 0.25), "{:?}", &l[..4]);
}

#[test]
fn a_reload_that_fails_calls_the_crash_hook() {
    let flag = std::env::temp_dir().join(format!("kadence-no-host-{}", std::process::id()));
    let _ = std::fs::remove_file(&flag);
    let crashes = Arc::new(AtomicUsize::new(0));
    let mut config = config();
    // A host that refuses to start once `flag` exists.
    config.launcher = HostLauncher::new(
        "/bin/sh",
        [
            "-c".into(),
            format!(
                "if [ -e \"$0\" ]; then exit 3; fi; exec '{}' \"$@\"",
                env!("CARGO_BIN_EXE_daw-plugin-host")
            ),
            flag.to_string_lossy().into_owned(),
        ],
    );
    {
        let crashes = crashes.clone();
        config.on_crash = Some(Arc::new(move |m| {
            assert!(m.is_crashed());
            assert_eq!(std::thread::current().name(), Some("plugin-watchdog"));
            crashes.fetch_add(1, Ordering::SeqCst);
        }));
    }
    let mut plugin = instance_with(config);
    let monitor = plugin.monitor();

    std::fs::write(&flag, b"").unwrap();
    monitor.request_reload();
    plugin.on_main_thread();
    wait_for("the crash hook", Duration::from_secs(5), || {
        crashes.load(Ordering::SeqCst) == 1
    });
    assert!(matches!(
        monitor.status(),
        SandboxStatus::Crashed(CrashReason::Lost(_))
    ));

    std::fs::remove_file(&flag).unwrap();
    plugin.reload().unwrap();
    assert_eq!(monitor.status(), SandboxStatus::Running);
    assert_eq!(crashes.load(Ordering::SeqCst), 1);
}

#[test]
fn names_taken_in_the_socket_directory_do_not_stop_plugins_loading() {
    // What another user could do in a shared /tmp: take the names we would use, in a way we
    // cannot remove.
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|d| d.is_dir())
        .unwrap_or_else(std::env::temp_dir);
    let taken: Vec<_> = (0..64)
        .map(|n| dir.join(format!("kadence-plugin-{}-{n}", std::process::id())))
        .collect();
    for path in &taken {
        std::fs::write(path, b"").unwrap();
    }
    let result = SandboxedInstance::new(descriptor(), config(), SR, MAX);
    for path in &taken {
        let _ = std::fs::remove_file(path);
    }
    let mut plugin = result.expect("load despite the taken names");
    let ([l, _], _) = run(&mut plugin, 16, 1.0, &[]);
    assert!(l.iter().all(|&s| s == 1.0));
}
