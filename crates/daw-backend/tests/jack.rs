//! Integration tests against a running JACK server. Ignored by default; CI
//! starts `jackd -d dummy` and runs them with `--run-ignored only`. Locally:
//!
//! ```sh
//! jackd -d dummy -r 48000 -p 256 &
//! cargo nextest run -p daw-backend --run-ignored only
//! ```

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use daw_backend::{
    AudioBackend, BackendConfig, BackendStats, JackBackend, PortDirection, PortFilter, PortKind,
};
use daw_core::event::{Event, EventKind};

#[global_allocator]
static ALLOC: daw_backend::RtAllocCounter = daw_backend::RtAllocCounter;

// JACK clients in one process share the server connection; keep tests apart.
static SERIAL: Mutex<()> = Mutex::new(());

fn wait(b: &JackBackend, pred: impl Fn(&BackendStats) -> bool) {
    let end = Instant::now() + Duration::from_secs(10);
    while !pred(&b.stats()) {
        assert!(Instant::now() < end, "timed out: {:?}", b.stats());
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn cfg(name: &str) -> BackendConfig {
    BackendConfig {
        client_name: name.into(),
        auto_connect: false,
        ..BackendConfig::default()
    }
}

#[test]
#[ignore = "needs a running JACK server"]
fn runs_cycles_and_lists_ports() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut b = JackBackend::new();
    b.open(cfg("wu2-basic")).unwrap();
    assert!(b.sample_rate() > 0.0);
    assert!(b.buffer_size() > 0);

    let mine: Vec<_> = b
        .list_ports(&PortFilter::default())
        .unwrap()
        .into_iter()
        .filter(|p| p.is_ours)
        .collect();
    let names: Vec<_> = mine.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(
        names.len(),
        5,
        "audio_in_1/2, audio_out_1/2, midi_in: {names:?}"
    );
    assert!(mine.iter().any(|p| p.name == "wu2-basic:midi_in"
        && p.kind == PortKind::Midi
        && p.direction == PortDirection::Input));

    let frames = Arc::new(AtomicU32::new(0));
    let f = frames.clone();
    b.start(Box::new(move |c| {
        f.store(c.frames, Relaxed);
        for out in c.audio_out.iter_mut() {
            out.fill(0.0);
        }
    }))
    .unwrap();
    wait(&b, |s| s.cycles >= 20);
    assert_eq!(frames.load(Relaxed), b.buffer_size());
    let s = b.stats();
    assert!(s.realtime.is_some());
    assert!(s.dsp_load.is_some());
    b.stop().unwrap();
    // Restart after stop.
    let before = b.stats().cycles;
    b.start(Box::new(|_| {})).unwrap();
    wait(&b, |s| s.cycles >= before + 5);
    assert_eq!(b.stats().rt_alloc_violations, 0);
    b.close().unwrap();
}

#[test]
#[ignore = "needs a running JACK server"]
fn audio_and_midi_flow_between_two_clients() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut src = JackBackend::new();
    // Every port kind in both directions on both sides, so the allocation
    // check covers the whole JACK cycle.
    src.open(BackendConfig {
        audio_inputs: 1,
        audio_outputs: 1,
        midi_input: true,
        midi_output: true,
        ..cfg("wu2-src")
    })
    .unwrap();
    let mut dst = JackBackend::new();
    dst.open(BackendConfig {
        audio_inputs: 1,
        audio_outputs: 1,
        midi_input: true,
        midi_output: true,
        ..cfg("wu2-dst")
    })
    .unwrap();

    src.start(Box::new(|c| {
        c.audio_out[0].fill(0.25);
        let on = Event {
            frame: 3,
            kind: EventKind::NoteOn {
                channel: 2,
                key: 64,
                velocity: 1.0,
                note_id: -1,
            },
        };
        c.midi_out.push(on).unwrap();
    }))
    .unwrap();

    let audio_ok = Arc::new(AtomicU64::new(0));
    let notes = Arc::new(AtomicU64::new(0));
    let (a, n) = (audio_ok.clone(), notes.clone());
    dst.start(Box::new(move |c| {
        c.audio_out[0].fill(0.0);
        if c.audio_in[0].iter().all(|&x| x == 0.25) {
            a.fetch_add(1, Relaxed);
        }
        for e in c.midi_in.iter() {
            if e.frame == 3
                && matches!(e.kind, EventKind::NoteOn { channel: 2, key: 64, velocity, .. } if velocity == 1.0)
            {
                n.fetch_add(1, Relaxed);
            }
        }
    }))
    .unwrap();

    src.connect("wu2-src:audio_out_1", "wu2-dst:audio_in_1")
        .unwrap();
    src.connect("wu2-src:midi_out", "wu2-dst:midi_in").unwrap();
    let end = Instant::now() + Duration::from_secs(10);
    while audio_ok.load(Relaxed) < 10 || notes.load(Relaxed) < 10 {
        assert!(Instant::now() < end, "audio {audio_ok:?}, notes {notes:?}");
        std::thread::sleep(Duration::from_millis(5));
    }
    src.disconnect("wu2-src:midi_out", "wu2-dst:midi_in")
        .unwrap();
    assert!(
        src.connect("wu2-dst:audio_in_1", "wu2-src:audio_out_1")
            .is_err()
    );
    assert_eq!(src.stats().rt_alloc_violations, 0);
    assert_eq!(dst.stats().rt_alloc_violations, 0);
    src.close().unwrap();
    dst.close().unwrap();
}

#[test]
#[ignore = "needs a running JACK server"]
fn auto_connect_reaches_physical_ports() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut b = JackBackend::new();
    b.open(BackendConfig {
        auto_connect: true,
        ..cfg("wu2-auto")
    })
    .unwrap();
    let physical_in = b
        .list_ports(&PortFilter {
            kind: Some(PortKind::Audio),
            direction: Some(PortDirection::Input),
            physical_only: true,
        })
        .unwrap();
    b.start(Box::new(|_| {})).unwrap();
    if let Some(first) = physical_in.first() {
        // Try to connect again: JACK reports the existing connection.
        assert!(b.connect("wu2-auto:audio_out_1", &first.name).is_err());
    }
    wait(&b, |s| s.cycles >= 5);
    assert_eq!(b.stats().rt_alloc_violations, 0);
    b.close().unwrap();
}

/// The connections of one port, read through a separate client the way a
/// patchbay sees them.
fn connections_of(port: &str) -> Vec<String> {
    let (probe, _) = jack::Client::new("wu2-probe", jack::ClientOptions::NO_START_SERVER).unwrap();
    let mut v = probe.port_by_name(port).unwrap().get_connections();
    v.sort();
    v
}

/// Waits for `port`'s connections to read `expected`: JACK shows a graph
/// change to other clients from its next cycle on.
fn wait_connections(port: &str, expected: &[&str]) {
    let end = Instant::now() + Duration::from_secs(2);
    loop {
        let now = connections_of(port);
        if now == expected {
            return;
        }
        assert!(
            Instant::now() < end,
            "{port} is connected to {now:?}, not {expected:?}"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
#[ignore = "needs a running JACK server"]
fn connections_survive_stop_and_start() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut b = JackBackend::new();
    b.open(BackendConfig {
        audio_inputs: 1,
        audio_outputs: 2,
        midi_input: false,
        ..cfg("wu2-keep")
    })
    .unwrap();
    // Before start: restoring saved routing must work on an inactive client.
    b.connect("wu2-keep:audio_out_1", "system:playback_2")
        .unwrap();
    // Bad connections are still refused before start.
    assert!(
        b.connect("system:playback_2", "wu2-keep:audio_out_1")
            .is_err()
    );
    assert!(b.connect("wu2-keep:audio_out_1", "nobody:nothing").is_err());
    b.start(Box::new(|_| {})).unwrap();
    assert_eq!(
        connections_of("wu2-keep:audio_out_1"),
        ["system:playback_2"]
    );

    // A connection made in a patchbay, and one made through us.
    {
        let (other, _) =
            jack::Client::new("wu2-patchbay", jack::ClientOptions::NO_START_SERVER).unwrap();
        other
            .connect_ports_by_name("system:capture_1", "wu2-keep:audio_in_1")
            .unwrap();
    }
    b.connect("wu2-keep:audio_out_2", "system:playback_1")
        .unwrap();

    b.stop().unwrap();
    b.start(Box::new(|_| {})).unwrap();
    wait_connections("wu2-keep:audio_out_1", &["system:playback_2"]);
    wait_connections("wu2-keep:audio_out_2", &["system:playback_1"]);
    wait_connections("wu2-keep:audio_in_1", &["system:capture_1"]);

    // A disconnect while stopped stays disconnected after start.
    b.stop().unwrap();
    b.disconnect("wu2-keep:audio_out_2", "system:playback_1")
        .unwrap();
    b.start(Box::new(|_| {})).unwrap();
    let cycles = b.stats().cycles;
    wait(&b, |s| s.cycles >= cycles + 5);
    assert!(connections_of("wu2-keep:audio_out_2").is_empty());
    assert_eq!(b.stats().rt_alloc_violations, 0);
    b.close().unwrap();
}

/// JACK shows a new link from its next cycle on, so a stop right after a
/// connect or a start must still keep the links it can't see yet.
#[test]
#[ignore = "needs a running JACK server"]
fn links_made_just_before_stop_are_kept() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut b = JackBackend::new();
    b.open(BackendConfig {
        audio_inputs: 0,
        audio_outputs: 1,
        midi_input: false,
        ..cfg("wu2-quick")
    })
    .unwrap();
    for _ in 0..20 {
        b.start(Box::new(|_| {})).unwrap();
        b.connect("wu2-quick:audio_out_1", "system:playback_1")
            .unwrap();
        b.stop().unwrap();
        // Restored on start, then stopped before a cycle shows it.
        b.start(Box::new(|_| {})).unwrap();
        b.stop().unwrap();
        b.start(Box::new(|_| {})).unwrap();
        wait_connections("wu2-quick:audio_out_1", &["system:playback_1"]);
        b.disconnect("wu2-quick:audio_out_1", "system:playback_1")
            .unwrap();
        b.stop().unwrap();
        b.start(Box::new(|_| {})).unwrap();
        let cycles = b.stats().cycles;
        wait(&b, |s| s.cycles >= cycles + 5);
        assert!(connections_of("wu2-quick:audio_out_1").is_empty());
        b.stop().unwrap();
    }
    assert_eq!(b.stats().rt_alloc_violations, 0);
    b.close().unwrap();
}

#[test]
#[ignore = "needs a running JACK server"]
fn a_panicking_callback_is_contained() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut b = JackBackend::new();
    b.open(cfg("wu2-panic")).unwrap();
    // A listener on our first output, counting cycles with sound in them.
    let mut ear = JackBackend::new();
    ear.open(BackendConfig {
        audio_inputs: 1,
        audio_outputs: 0,
        midi_input: false,
        midi_output: false,
        ..cfg("wu2-ear")
    })
    .unwrap();
    let loud = Arc::new(AtomicU64::new(0));
    let l2 = loud.clone();
    ear.start(Box::new(move |c| {
        if c.audio_in[0].iter().any(|&x| x != 0.0) {
            l2.fetch_add(1, Relaxed);
        }
    }))
    .unwrap();
    let calls = Arc::new(AtomicU64::new(0));
    let c2 = calls.clone();
    b.start(Box::new(move |c| {
        for out in c.audio_out.iter_mut() {
            out.fill(0.5);
        }
        if c2.fetch_add(1, Relaxed) == 10 {
            panic!("callback bug");
        }
    }))
    .unwrap();
    ear.connect("wu2-panic:audio_out_1", "wu2-ear:audio_in_1")
        .unwrap();
    wait(&b, |s| s.callback_panicked);
    // The backend keeps running (silently) and no longer calls the callback.
    let after = b.stats().cycles;
    wait(&b, |s| s.cycles >= after + 2);
    let loud_after = loud.load(Relaxed);
    wait(&b, |s| s.cycles >= after + 12);
    assert_eq!(calls.load(Relaxed), 11);
    assert!(b.is_running());
    // The panicking cycle's half-written output is not left playing.
    assert_eq!(loud.load(Relaxed), loud_after, "output not silenced");
    ear.close().unwrap();
    // Teardown is clean, and a new callback starts afresh.
    b.stop().unwrap();
    let before = b.stats().cycles;
    b.start(Box::new(|_| {})).unwrap();
    wait(&b, |s| s.cycles >= before + 5);
    assert!(!b.stats().callback_panicked);
    b.close().unwrap();
}

/// Connecting two other clients' ports needs nothing from us, so it happens
/// at once even while we are stopped, and is not tied to our start and stop.
#[test]
#[ignore = "needs a running JACK server"]
fn connects_other_clients_ports_while_stopped() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut b = JackBackend::new();
    b.open(cfg("wu2-route")).unwrap();
    // Other tests may link system ports at the same time (nextest runs each
    // test in its own process, so SERIAL does not cover them): only look at
    // this pair.
    let linked = || connections_of("system:capture_1").contains(&"system:playback_1".to_string());
    b.connect("system:capture_1", "system:playback_1").unwrap();
    assert!(linked());
    b.disconnect("system:capture_1", "system:playback_1")
        .unwrap();
    assert!(!linked());
    b.close().unwrap();
}

/// Saved links are kept by full port name, so a port named by its alias in
/// one call and by its full name in the next is the same port.
#[test]
#[ignore = "needs a running JACK server"]
fn stopped_links_match_aliases_and_loopbacks() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let (probe, _) =
        jack::Client::new("wu2-alias-probe", jack::ClientOptions::NO_START_SERVER).unwrap();
    let mut sys = probe.port_by_name("system:playback_1").unwrap();
    sys.set_alias("wu2-test:speaker").unwrap();

    let mut b = JackBackend::new();
    b.open(BackendConfig {
        audio_inputs: 1,
        audio_outputs: 1,
        midi_input: false,
        ..cfg("wu2-alias")
    })
    .unwrap();
    b.connect("wu2-alias:audio_out_1", "wu2-test:speaker")
        .unwrap();
    // The same link under its full name.
    assert!(
        b.connect("wu2-alias:audio_out_1", "system:playback_1")
            .is_err()
    );
    b.disconnect("wu2-alias:audio_out_1", "system:playback_1")
        .unwrap();
    // A link from our output to our own input.
    b.connect("wu2-alias:audio_out_1", "wu2-alias:audio_in_1")
        .unwrap();
    b.start(Box::new(|_| {})).unwrap();
    assert_eq!(
        connections_of("wu2-alias:audio_out_1"),
        ["wu2-alias:audio_in_1"]
    );
    b.stop().unwrap();
    b.start(Box::new(|_| {})).unwrap();
    assert_eq!(
        connections_of("wu2-alias:audio_out_1"),
        ["wu2-alias:audio_in_1"]
    );
    b.close().unwrap();
    sys.unset_alias("wu2-test:speaker").unwrap();
}

#[test]
#[ignore = "needs a running JACK server"]
fn added_midi_inputs_keep_their_sources_apart() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut src = JackBackend::new();
    src.open(BackendConfig {
        midi_output: true,
        ..cfg("wu10-src")
    })
    .unwrap();
    src.start(Box::new(|c| {
        let on = Event {
            frame: 1,
            kind: EventKind::NoteOn {
                channel: 0,
                key: 61,
                velocity: 1.0,
                note_id: -1,
            },
        };
        c.midi_out.push(on).unwrap();
    }))
    .unwrap();

    let mut dst = JackBackend::new();
    dst.open(cfg("wu10-dst")).unwrap();
    // One port before start, one while running.
    let before = dst.add_midi_input("keys").unwrap();
    assert_eq!(before.name, "wu10-dst:keys");
    let counts: Arc<Vec<AtomicU64>> = Arc::new(
        (0..daw_backend::MAX_MIDI_INPUTS)
            .map(|_| AtomicU64::new(0))
            .collect(),
    );
    let general = Arc::new(AtomicU64::new(0));
    let (c2, g2) = (counts.clone(), general.clone());
    dst.start(Box::new(move |c| {
        g2.fetch_add(c.midi_in.len() as u64, Relaxed);
        for (slot, list) in c.midi_inputs.iter().enumerate() {
            c2[slot].fetch_add(list.len() as u64, Relaxed);
        }
    }))
    .unwrap();
    let during = dst.add_midi_input("pads").unwrap();
    dst.connect("wu10-src:midi_out", &during.name).unwrap();
    let end = Instant::now() + Duration::from_secs(10);
    while counts[during.slot].load(Relaxed) < 10 {
        assert!(Instant::now() < end, "no MIDI on the added port");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(counts[before.slot].load(Relaxed), 0);
    assert_eq!(general.load(Relaxed), 0);

    // A link made while stopped comes back on start, and a removed port's
    // links don't.
    dst.stop().unwrap();
    dst.connect("wu10-src:midi_out", &before.name).unwrap();
    dst.remove_midi_input(during.slot).unwrap();
    dst.start(Box::new(|_| {})).unwrap();
    assert_eq!(connections_of(&before.name), ["wu10-src:midi_out"]);
    assert!(
        dst.list_ports(&PortFilter::default())
            .unwrap()
            .iter()
            .all(|p| p.name != during.name),
        "the removed port is unregistered"
    );
    assert_eq!(dst.stats().rt_alloc_violations, 0);
    src.close().unwrap();
    dst.close().unwrap();
}

struct Idle;

impl jack::ProcessHandler for Idle {
    fn process(&mut self, _: &jack::Client, _: &jack::ProcessScope) -> jack::Control {
        jack::Control::Continue
    }
}

/// A link between two other clients made through us isn't ours to keep: a
/// disconnect while stopped must reach JACK, not just a saved list.
#[test]
#[ignore = "needs a running JACK server"]
fn other_clients_links_are_not_saved_over_a_stop() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let (other, _) = jack::Client::new("wu2-other", jack::ClientOptions::NO_START_SERVER).unwrap();
    let _input = other.register_port("in", jack::AudioIn::default()).unwrap();
    let other = other.activate_async((), Idle).unwrap();
    let mut b = JackBackend::new();
    b.open(BackendConfig {
        audio_inputs: 0,
        audio_outputs: 1,
        midi_input: false,
        ..cfg("wu2-foreign")
    })
    .unwrap();
    b.start(Box::new(|_| {})).unwrap();
    b.connect("system:capture_1", "wu2-other:in").unwrap();
    b.stop().unwrap();
    wait_connections("wu2-other:in", &["system:capture_1"]);
    b.disconnect("system:capture_1", "wu2-other:in").unwrap();
    wait_connections("wu2-other:in", &[]);
    b.close().unwrap();
    drop(other);
}

/// A patchbay that undoes our link right away has the last word.
#[test]
#[ignore = "needs a running JACK server"]
fn a_patchbay_change_after_ours_is_kept() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let (patchbay, _) =
        jack::Client::new("wu2-patchbay2", jack::ClientOptions::NO_START_SERVER).unwrap();
    let mut b = JackBackend::new();
    b.open(BackendConfig {
        audio_inputs: 0,
        audio_outputs: 1,
        midi_input: false,
        ..cfg("wu2-undone")
    })
    .unwrap();
    for _ in 0..10 {
        b.start(Box::new(|_| {})).unwrap();
        b.connect("wu2-undone:audio_out_1", "system:playback_1")
            .unwrap();
        patchbay
            .disconnect_ports_by_name("wu2-undone:audio_out_1", "system:playback_1")
            .unwrap();
        b.stop().unwrap();
        b.start(Box::new(|_| {})).unwrap();
        let cycles = b.stats().cycles;
        wait(&b, |s| s.cycles >= cycles + 5);
        assert!(connections_of("wu2-undone:audio_out_1").is_empty());
        b.stop().unwrap();
    }
    b.close().unwrap();
}
