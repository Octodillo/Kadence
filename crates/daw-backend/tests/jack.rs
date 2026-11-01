//! Integration tests against a running JACK server: a real `jackd` or
//! PipeWire's `libjack` (`pipewire-jack`). Ignored by default; CI runs them
//! on both, with `--run-ignored only`. Locally:
//!
//! ```sh
//! jackd -d dummy -r 48000 -p 256 &
//! cargo nextest run -p daw-backend --run-ignored only
//! # or, on a desktop with PipeWire:
//! pw-jack cargo nextest run -p daw-backend --run-ignored only
//! ```
//!
//! On a desktop the physical ports are the real speakers and microphone,
//! so these tests never link them: [`FakeHardware`] stands in for them.
//! The one test that needs real physical ports runs only where
//! `KADENCE_TEST_PHYSICAL_PORTS=1` says they are a dummy server's.
//!
//! The servers differ in ways the tests allow for:
//! - PipeWire shows a client's ports, links and aliases to other clients a
//!   moment later, not at once, so tests wait for what they are about to use
//!   ([`wait_visible`], [`wait_connections`]).
//! - PipeWire names a hardware port after its device, and answers to
//!   `system:playback_1` by mapping it to the default output. Tests never
//!   use `system:` names.
//! - PipeWire does not rename a second client with a name already taken, and
//!   nextest runs each test in its own process, so every client name here
//!   carries the process id ([`unique`]).

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use daw_backend::{
    AudioBackend, BackendConfig, BackendStats, JackBackend, PortDirection, PortFilter, PortKind,
};
use daw_core::event::{Event, EventKind};

#[global_allocator]
static ALLOC: daw_backend::RtAllocCounter = daw_backend::RtAllocCounter;

// JACK clients in one process share the server connection; keep tests apart
// when they run in one process (`cargo test`).
static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

/// A client name no other test process has: `kt-<base>-<pid>`.
fn unique(base: &str) -> String {
    format!("kt-{base}-{}", std::process::id())
}

/// Whether `done` turns true within `timeout`.
fn within(timeout: Duration, mut done: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + timeout;
    while !done() {
        if Instant::now() >= end {
            return false;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    true
}

/// Waits up to ten seconds for `done`, then fails with `what()`.
fn eventually(done: impl FnMut() -> bool, what: impl Fn() -> String) {
    assert!(
        within(Duration::from_secs(10), done),
        "timed out: {}",
        what()
    );
}

fn wait(b: &JackBackend, pred: impl Fn(&BackendStats) -> bool) {
    eventually(|| pred(&b.stats()), || format!("{:?}", b.stats()));
}

/// Waits for `b` to see another client's `port`, so it can link it.
fn wait_visible(b: &JackBackend, port: &str) {
    let names = || -> Vec<String> {
        b.list_ports(&PortFilter::default())
            .unwrap()
            .into_iter()
            .map(|p| p.name)
            .collect()
    };
    eventually(
        || names().iter().any(|n| n == port),
        || format!("{port} not among {:?}", names()),
    );
}

/// A client that only looks, the way a patchbay does.
struct Probe(jack::Client);

impl Probe {
    fn new() -> Self {
        let (c, _) = jack::Client::new(&unique("probe"), jack::ClientOptions::NO_START_SERVER)
            .expect("a JACK server is running");
        Self(c)
    }

    fn connections_of(&self, port: &str) -> Vec<String> {
        let mut v = self
            .0
            .port_by_name(port)
            .map(|p| p.get_connections())
            .unwrap_or_default();
        v.sort();
        v
    }
}

/// Waits for `port`'s connections to read `expected`: the server shows a
/// graph change to other clients a cycle or two later.
fn wait_connections(port: &str, expected: &[&str]) {
    let probe = Probe::new();
    eventually(
        || probe.connections_of(port) == expected,
        || {
            format!(
                "{port} is connected to {:?}, not {expected:?}",
                probe.connections_of(port)
            )
        },
    );
}

/// Waits a few cycles of `b`, then checks `port` has no connections. For
/// links that must *not* come back: give them the time they would take.
fn assert_stays_unlinked(b: &JackBackend, port: &str) {
    let cycles = b.stats().cycles;
    wait(b, |s| s.cycles >= cycles + 5);
    let probe = Probe::new();
    // Leave PipeWire's graph time to show a link made in those cycles.
    std::thread::sleep(Duration::from_millis(50));
    assert!(
        probe.connections_of(port).is_empty(),
        "{port} is connected to {:?}",
        probe.connections_of(port)
    );
}

struct Idle;

impl jack::ProcessHandler for Idle {
    fn process(&mut self, _: &jack::Client, _: &jack::ProcessScope) -> jack::Control {
        jack::Control::Continue
    }
}

/// Stands in for a sound card: an active client with two capture ports
/// (outputs) and two playback ports (inputs). Unlike the real ones, nobody
/// hears what reaches it.
struct FakeHardware {
    name: String,
    client: jack::AsyncClient<(), Idle>,
    _capture: Vec<jack::Port<jack::AudioOut>>,
    playback: Vec<jack::Port<jack::AudioIn>>,
}

impl FakeHardware {
    fn new() -> Self {
        let name = unique("hw");
        let (client, _) = jack::Client::new(&name, jack::ClientOptions::NO_START_SERVER)
            .expect("a JACK server is running");
        let capture = (1..=2)
            .map(|i| {
                client
                    .register_port(&format!("capture_{i}"), jack::AudioOut::default())
                    .unwrap()
            })
            .collect();
        let playback = (1..=2)
            .map(|i| {
                client
                    .register_port(&format!("playback_{i}"), jack::AudioIn::default())
                    .unwrap()
            })
            .collect();
        let client = client.activate_async((), Idle).unwrap();
        Self {
            name,
            client,
            _capture: capture,
            playback,
        }
    }

    fn capture(&self, i: usize) -> String {
        format!("{}:capture_{i}", self.name)
    }

    fn playback(&self, i: usize) -> String {
        format!("{}:playback_{i}", self.name)
    }

    /// Disconnects as another client would, once it sees the link.
    fn patchbay_disconnect(&self, from: &str, to: &str) {
        let client = self.client.as_client();
        eventually(
            || {
                client
                    .port_by_name(from)
                    .is_some_and(|p| p.is_connected_to(to).unwrap_or(false))
            },
            || format!("the patchbay never saw {from} -> {to}"),
        );
        client.disconnect_ports_by_name(from, to).unwrap();
    }
}

/// The defaults, which connect nothing to the hardware, under `name`.
fn cfg(name: &str) -> BackendConfig {
    BackendConfig {
        client_name: name.into(),
        ..BackendConfig::default()
    }
}

#[test]
#[ignore = "needs a running JACK server"]
fn runs_cycles_and_lists_ports() {
    let _g = serial();
    let me = unique("basic");
    let mut b = JackBackend::new();
    b.open(cfg(&me)).unwrap();
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
    assert!(mine.iter().any(|p| p.name == format!("{me}:midi_in")
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
    let _g = serial();
    let (s, d) = (unique("src"), unique("dst"));
    let mut src = JackBackend::new();
    // Every port kind in both directions on both sides, so the allocation
    // check covers the whole JACK cycle.
    let both_ways = |name: &str| BackendConfig {
        audio_inputs: 1,
        audio_outputs: 1,
        midi_input: true,
        midi_output: true,
        ..cfg(name)
    };
    src.open(both_ways(&s)).unwrap();
    let mut dst = JackBackend::new();
    dst.open(both_ways(&d)).unwrap();

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

    let (audio_out, audio_in) = (format!("{s}:audio_out_1"), format!("{d}:audio_in_1"));
    let (midi_out, midi_in) = (format!("{s}:midi_out"), format!("{d}:midi_in"));
    wait_visible(&src, &audio_in);
    wait_visible(&src, &midi_in);
    src.connect(&audio_out, &audio_in).unwrap();
    src.connect(&midi_out, &midi_in).unwrap();
    eventually(
        || audio_ok.load(Relaxed) >= 10 && notes.load(Relaxed) >= 10,
        || format!("audio {audio_ok:?}, notes {notes:?}"),
    );
    src.disconnect(&midi_out, &midi_in).unwrap();
    // Input to output is refused.
    assert!(src.connect(&audio_in, &audio_out).is_err());
    assert_eq!(src.stats().rt_alloc_violations, 0);
    assert_eq!(dst.stats().rt_alloc_violations, 0);
    src.close().unwrap();
    dst.close().unwrap();
}

/// Only where the physical ports are a dummy server's: anywhere else they
/// are someone's speakers.
#[test]
#[ignore = "needs a running JACK server"]
fn auto_connect_reaches_physical_ports() {
    if std::env::var("KADENCE_TEST_PHYSICAL_PORTS").as_deref() != Ok("1") {
        eprintln!("skipped: set KADENCE_TEST_PHYSICAL_PORTS=1 on a jackd -d dummy server");
        return;
    }
    let _g = serial();
    let me = unique("auto");
    let mut b = JackBackend::new();
    b.open(BackendConfig {
        auto_connect: true,
        ..cfg(&me)
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
    let first = physical_in
        .first()
        .expect("the dummy server has playback ports");
    let out = format!("{me}:audio_out_1");
    wait_connections(&out, &[first.name.as_str()]);
    // Connecting again is refused: the link is there.
    assert!(b.connect(&out, &first.name).is_err());
    wait(&b, |s| s.cycles >= 5);
    assert_eq!(b.stats().rt_alloc_violations, 0);
    b.close().unwrap();
}

#[test]
#[ignore = "needs a running JACK server"]
fn connections_survive_stop_and_start() {
    let _g = serial();
    let hw = FakeHardware::new();
    let me = unique("keep");
    let (in1, out1, out2) = (
        format!("{me}:audio_in_1"),
        format!("{me}:audio_out_1"),
        format!("{me}:audio_out_2"),
    );
    let mut b = JackBackend::new();
    b.open(BackendConfig {
        audio_inputs: 1,
        audio_outputs: 2,
        midi_input: false,
        ..cfg(&me)
    })
    .unwrap();
    wait_visible(&b, &hw.playback(2));
    // Before start: restoring saved routing must work on an inactive client.
    b.connect(&out1, &hw.playback(2)).unwrap();
    // Bad connections are still refused before start.
    assert!(b.connect(&hw.playback(2), &out1).is_err());
    assert!(b.connect(&out1, "nobody:nothing").is_err());
    b.start(Box::new(|_| {})).unwrap();
    wait_connections(&out1, &[&hw.playback(2)]);

    // A connection made in a patchbay, and one made through us.
    {
        let (other, _) =
            jack::Client::new(&unique("patchbay"), jack::ClientOptions::NO_START_SERVER).unwrap();
        other.connect_ports_by_name(&hw.capture(1), &in1).unwrap();
    }
    b.connect(&out2, &hw.playback(1)).unwrap();

    b.stop().unwrap();
    b.start(Box::new(|_| {})).unwrap();
    wait_connections(&out1, &[&hw.playback(2)]);
    wait_connections(&out2, &[&hw.playback(1)]);
    wait_connections(&in1, &[&hw.capture(1)]);

    // A disconnect while stopped stays disconnected after start.
    b.stop().unwrap();
    b.disconnect(&out2, &hw.playback(1)).unwrap();
    b.start(Box::new(|_| {})).unwrap();
    assert_stays_unlinked(&b, &out2);
    assert_eq!(b.stats().rt_alloc_violations, 0);
    b.close().unwrap();
}

/// JACK shows a new link from its next cycle on, so a stop right after a
/// connect or a start must still keep the links it can't see yet.
#[test]
#[ignore = "needs a running JACK server"]
fn links_made_just_before_stop_are_kept() {
    let _g = serial();
    let hw = FakeHardware::new();
    let me = unique("quick");
    let out = format!("{me}:audio_out_1");
    let speaker = hw.playback(1);
    let mut b = JackBackend::new();
    b.open(BackendConfig {
        audio_inputs: 0,
        audio_outputs: 1,
        midi_input: false,
        ..cfg(&me)
    })
    .unwrap();
    wait_visible(&b, &speaker);
    for _ in 0..20 {
        b.start(Box::new(|_| {})).unwrap();
        b.connect(&out, &speaker).unwrap();
        b.stop().unwrap();
        // Restored on start, then stopped before a cycle shows it.
        b.start(Box::new(|_| {})).unwrap();
        b.stop().unwrap();
        b.start(Box::new(|_| {})).unwrap();
        wait_connections(&out, &[&speaker]);
        b.disconnect(&out, &speaker).unwrap();
        b.stop().unwrap();
        b.start(Box::new(|_| {})).unwrap();
        assert_stays_unlinked(&b, &out);
        b.stop().unwrap();
    }
    assert_eq!(b.stats().rt_alloc_violations, 0);
    b.close().unwrap();
}

#[test]
#[ignore = "needs a running JACK server"]
fn a_panicking_callback_is_contained() {
    let _g = serial();
    let (me, listener) = (unique("panic"), unique("ear"));
    let mut b = JackBackend::new();
    b.open(cfg(&me)).unwrap();
    // A listener on our first output, counting cycles with sound in them.
    let mut ear = JackBackend::new();
    ear.open(BackendConfig {
        audio_inputs: 1,
        audio_outputs: 0,
        midi_input: false,
        midi_output: false,
        ..cfg(&listener)
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
    let out = format!("{me}:audio_out_1");
    wait_visible(&ear, &out);
    ear.connect(&out, &format!("{listener}:audio_in_1"))
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
    let _g = serial();
    let hw = FakeHardware::new();
    let mut b = JackBackend::new();
    b.open(cfg(&unique("route"))).unwrap();
    let (mic, speaker) = (hw.capture(1), hw.playback(1));
    wait_visible(&b, &mic);
    wait_visible(&b, &speaker);
    b.connect(&mic, &speaker).unwrap();
    wait_connections(&mic, &[&speaker]);
    b.disconnect(&mic, &speaker).unwrap();
    wait_connections(&mic, &[]);
    b.close().unwrap();
}

/// Saved links are kept by full port name, so a port named by its alias in
/// one call and by its full name in the next is the same port.
///
/// PipeWire shows other clients' aliases only with `jack.fill-aliases` on,
/// and only those set before the port appeared; without them it checks the
/// loopback alone.
#[test]
#[ignore = "needs a running JACK server"]
fn stopped_links_match_aliases_and_loopbacks() {
    let _g = serial();
    let mut hw = FakeHardware::new();
    let alias = format!("{}:speaker", unique("alias-of"));
    hw.playback[0].set_alias(&alias).unwrap();
    let speaker = hw.playback(1);
    let probe = Probe::new();
    let aliases_shown = within(Duration::from_secs(1), || {
        probe.0.port_by_name(&alias).is_some()
    });

    let me = unique("alias");
    let (out, input) = (format!("{me}:audio_out_1"), format!("{me}:audio_in_1"));
    let mut b = JackBackend::new();
    b.open(BackendConfig {
        audio_inputs: 1,
        audio_outputs: 1,
        midi_input: false,
        ..cfg(&me)
    })
    .unwrap();
    if aliases_shown {
        b.connect(&out, &alias).unwrap();
        // The same link under its full name.
        assert!(b.connect(&out, &speaker).is_err());
        b.disconnect(&out, &speaker).unwrap();
    } else {
        eprintln!("this server does not show other clients' aliases; checking the loopback only");
    }
    // A link from our output to our own input.
    b.connect(&out, &input).unwrap();
    b.start(Box::new(|_| {})).unwrap();
    wait_connections(&out, &[&input]);
    b.stop().unwrap();
    b.start(Box::new(|_| {})).unwrap();
    wait_connections(&out, &[&input]);
    b.close().unwrap();
}

#[test]
#[ignore = "needs a running JACK server"]
fn added_midi_inputs_keep_their_sources_apart() {
    let _g = serial();
    let (s, d) = (unique("msrc"), unique("mdst"));
    let mut src = JackBackend::new();
    src.open(BackendConfig {
        midi_output: true,
        ..cfg(&s)
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
    let source = format!("{s}:midi_out");

    let mut dst = JackBackend::new();
    dst.open(cfg(&d)).unwrap();
    // One port before start, one while running.
    let before = dst.add_midi_input("keys").unwrap();
    assert_eq!(before.name, format!("{d}:keys"));
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
    wait_visible(&dst, &source);
    dst.connect(&source, &during.name).unwrap();
    eventually(
        || counts[during.slot].load(Relaxed) >= 10,
        || "no MIDI on the added port".into(),
    );
    assert_eq!(counts[before.slot].load(Relaxed), 0);
    assert_eq!(general.load(Relaxed), 0);

    // A link made while stopped comes back on start, and a removed port's
    // links don't.
    dst.stop().unwrap();
    dst.connect(&source, &before.name).unwrap();
    dst.remove_midi_input(during.slot).unwrap();
    dst.start(Box::new(|_| {})).unwrap();
    wait_connections(&before.name, &[&source]);
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

/// A link between two other clients made through us isn't ours to keep: a
/// disconnect while stopped must reach JACK, not just a saved list.
#[test]
#[ignore = "needs a running JACK server"]
fn other_clients_links_are_not_saved_over_a_stop() {
    let _g = serial();
    let hw = FakeHardware::new();
    let mic = hw.capture(1);
    let other_name = unique("other");
    let (other, _) = jack::Client::new(&other_name, jack::ClientOptions::NO_START_SERVER).unwrap();
    let _input = other.register_port("in", jack::AudioIn::default()).unwrap();
    let other = other.activate_async((), Idle).unwrap();
    let other_in = format!("{other_name}:in");
    let mut b = JackBackend::new();
    b.open(BackendConfig {
        audio_inputs: 0,
        audio_outputs: 1,
        midi_input: false,
        ..cfg(&unique("foreign"))
    })
    .unwrap();
    b.start(Box::new(|_| {})).unwrap();
    wait_visible(&b, &mic);
    wait_visible(&b, &other_in);
    b.connect(&mic, &other_in).unwrap();
    b.stop().unwrap();
    wait_connections(&other_in, &[&mic]);
    b.disconnect(&mic, &other_in).unwrap();
    wait_connections(&other_in, &[]);
    b.close().unwrap();
    drop(other);
}

/// A patchbay that undoes our link right away has the last word.
///
/// Its many stops and starts are what tripped a pipewire-jack 1.0 crash
/// ("invalid mem id", then SIGSEGV) when other tests ran in parallel, which
/// is why `ci/pipewire-jack-tests.sh` runs one test at a time.
#[test]
#[ignore = "needs a running JACK server"]
fn a_patchbay_change_after_ours_is_kept() {
    let _g = serial();
    let hw = FakeHardware::new();
    let me = unique("undone");
    let out = format!("{me}:audio_out_1");
    let speaker = hw.playback(1);
    let mut b = JackBackend::new();
    b.open(BackendConfig {
        audio_inputs: 0,
        audio_outputs: 1,
        midi_input: false,
        ..cfg(&me)
    })
    .unwrap();
    wait_visible(&b, &speaker);
    for _ in 0..10 {
        b.start(Box::new(|_| {})).unwrap();
        b.connect(&out, &speaker).unwrap();
        hw.patchbay_disconnect(&out, &speaker);
        b.stop().unwrap();
        b.start(Box::new(|_| {})).unwrap();
        assert_stays_unlinked(&b, &out);
        b.stop().unwrap();
    }
    b.close().unwrap();
}
