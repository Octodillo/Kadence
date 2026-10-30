use std::sync::mpsc;
use std::time::{Duration, Instant};

use daw_backend::{
    AudioBackend, BackendConfig, BackendError, DummyBackend, Latency, Pacing, PortDirection,
    PortFilter, PortKind,
};
use daw_core::event::{Event, EventKind};

const WAIT: Duration = Duration::from_secs(10);

fn cfg(frames: u32) -> BackendConfig {
    BackendConfig {
        client_name: "test".into(),
        audio_inputs: 1,
        audio_outputs: 2,
        midi_input: true,
        midi_output: true,
        sample_rate: Some(48_000),
        buffer_size: Some(frames),
        auto_connect: false,
        midi_capacity: 16,
    }
}

#[test]
fn freewheel_runs_cycles_with_correct_buffers() {
    let mut b = DummyBackend::new(Pacing::Freewheel).with_max_cycles(100);
    b.open(cfg(64)).unwrap();
    assert_eq!(b.sample_rate(), 48_000.0);
    assert_eq!(b.buffer_size(), 64);
    let (tx, rx) = mpsc::sync_channel(200);
    b.start(Box::new(move |c| {
        assert_eq!(c.audio_in.len(), 1);
        assert_eq!(c.audio_out.len(), 2);
        assert!(
            c.audio_in
                .iter()
                .all(|s| s.len() == 64 && s.iter().all(|&x| x == 0.0))
        );
        for out in c.audio_out.iter_mut() {
            assert_eq!(out.len(), c.frames as usize);
            out.fill(0.5);
        }
        let _ = tx.try_send((c.frames, c.frame_time, c.sample_rate));
    }))
    .unwrap();
    assert!(b.wait_for_cycles(100, WAIT));
    b.stop().unwrap();
    let seen: Vec<_> = rx.try_iter().collect();
    assert_eq!(seen.len(), 100);
    for (i, (frames, time, rate)) in seen.into_iter().enumerate() {
        assert_eq!((frames, time, rate), (64, i as u64 * 64, 48_000.0));
    }
    let s = b.stats();
    assert_eq!((s.cycles, s.frames, s.xruns), (100, 6400, 0));
    assert!(s.realtime.is_some());
}

#[test]
fn midi_in_is_decoded_and_midi_out_is_encoded() {
    let mut b = DummyBackend::new(Pacing::Freewheel);
    b.open(cfg(32)).unwrap();
    let mut inject = b.midi_injector();
    let mut tap = b.midi_tap();
    assert!(inject.send(&[0x91, 60, 127]));
    assert!(!inject.send(&[0xF0, 1, 2, 0xF7]), "too long");
    assert!(inject.send(&[0xB0, 7, 99]));

    let (tx, rx) = mpsc::sync_channel(16);
    b.start(Box::new(move |c| {
        let got: Vec<Event> = c.midi_in.iter().copied().collect();
        if !got.is_empty() {
            let _ = tx.try_send(got);
            let echo = Event {
                frame: 5,
                kind: EventKind::NoteOff {
                    channel: 1,
                    key: Some(60),
                    velocity: 0.0,
                    note_id: -1,
                },
            };
            c.midi_out.push(echo).unwrap();
        }
    }))
    .unwrap();
    let got = rx.recv_timeout(WAIT).unwrap();
    assert_eq!(
        got,
        vec![
            Event {
                frame: 0,
                kind: EventKind::NoteOn {
                    channel: 1,
                    key: 60,
                    velocity: 1.0,
                    note_id: -1
                }
            },
            Event {
                frame: 0,
                kind: EventKind::Midi([0xB0, 7, 99])
            },
        ]
    );
    let end = Instant::now() + WAIT;
    let out = loop {
        if let Some(m) = tap.try_recv() {
            break m;
        }
        assert!(Instant::now() < end, "no MIDI out");
        std::thread::yield_now();
    };
    assert_eq!(out.as_bytes(), &[0x81, 60, 0]);
    assert_eq!(out.frame % 32, 5);
    b.stop().unwrap();
}

#[test]
fn midi_overflow_is_counted_not_fatal() {
    let mut b = DummyBackend::new(Pacing::Freewheel);
    b.open(cfg(32)).unwrap(); // midi_capacity 16
    let mut inject = b.midi_injector();
    for k in 0..20 {
        assert!(inject.send(&[0x90, k, 100]));
    }
    b.start(Box::new(|_| {})).unwrap();
    assert!(b.wait_for_cycles(5, WAIT));
    b.stop().unwrap();
    assert_eq!(b.stats().midi_dropped, 4);
}

#[test]
fn malformed_midi_out_is_dropped_and_counted() {
    let mut b = DummyBackend::new(Pacing::Freewheel).with_max_cycles(1);
    b.open(cfg(32)).unwrap();
    let mut tap = b.midi_tap();
    b.start(Box::new(|c| {
        for kind in [
            EventKind::Midi([0xB0, 7, 200]), // status bit in a data byte
            EventKind::Midi([0xF0, 1, 2]),   // SysEx has no short form
            EventKind::Midi([0xB0, 7, 100]),
            EventKind::ParamValue {
                param: 0,
                value: 1.0,
            },
        ] {
            c.midi_out.push(Event { frame: 0, kind }).unwrap();
        }
    }))
    .unwrap();
    assert!(b.wait_for_cycles(1, WAIT));
    b.stop().unwrap();
    let sent: Vec<_> = std::iter::from_fn(|| tap.try_recv()).collect();
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(sent[0].as_bytes(), &[0xB0, 7, 100]);
    assert_eq!(b.stats().midi_dropped, 2, "ParamValue is not a drop");
}

#[test]
fn realtime_pacing_keeps_time() {
    // 20 cycles of 480 frames at 48 kHz = 200 ms.
    let mut b = DummyBackend::new(Pacing::RealTime);
    b.open(cfg(480)).unwrap();
    let t = Instant::now();
    b.start(Box::new(|_| {})).unwrap();
    assert!(b.wait_for_cycles(20, WAIT));
    let elapsed = t.elapsed();
    b.stop().unwrap();
    assert!(
        elapsed >= Duration::from_millis(180),
        "too fast: {elapsed:?}"
    );
    assert!(elapsed < Duration::from_secs(2), "too slow: {elapsed:?}");
}

#[test]
fn slow_callback_counts_xruns() {
    let mut b = DummyBackend::new(Pacing::RealTime);
    b.open(cfg(48)).unwrap(); // 1 ms period
    b.start(Box::new(|c| {
        if c.frame_time / 48 % 2 == 0 {
            std::thread::sleep(Duration::from_millis(3));
        }
    }))
    .unwrap();
    assert!(b.wait_for_cycles(10, WAIT));
    b.stop().unwrap();
    let s = b.stats();
    assert!(s.xruns >= 3, "{s:?}");
    assert!(s.dsp_load.is_some());
}

#[test]
fn ports_list_and_connect() {
    let mut b = DummyBackend::new(Pacing::Freewheel);
    assert!(matches!(
        b.list_ports(&PortFilter::default()),
        Err(BackendError::NotOpen)
    ));
    b.open(cfg(64)).unwrap();

    let ours: Vec<_> = b
        .list_ports(&PortFilter::default())
        .unwrap()
        .into_iter()
        .filter(|p| p.is_ours)
        .map(|p| p.name)
        .collect();
    assert_eq!(
        ours,
        [
            "test:audio_in_1",
            "test:audio_out_1",
            "test:audio_out_2",
            "test:midi_in",
            "test:midi_out"
        ]
    );

    let playback = b
        .list_ports(&PortFilter {
            kind: Some(PortKind::Audio),
            direction: Some(PortDirection::Input),
            physical_only: true,
        })
        .unwrap();
    assert_eq!(playback.len(), 2);
    assert!(playback.iter().all(|p| p.is_physical && !p.is_ours));

    b.connect("test:audio_out_1", "system:playback_1").unwrap();
    assert!(
        b.connect("system:playback_1", "test:audio_out_1").is_err(),
        "wrong direction"
    );
    assert!(
        b.connect("test:midi_out", "system:playback_2").is_err(),
        "type mismatch"
    );
    assert!(
        b.connect("test:nope", "system:playback_2").is_err(),
        "no such port"
    );
    assert_eq!(b.connections().count(), 1);
    b.disconnect("test:audio_out_1", "system:playback_1")
        .unwrap();
    assert!(
        b.disconnect("test:audio_out_1", "system:playback_1")
            .is_err()
    );
}

#[test]
fn auto_connect_wires_physical_ports() {
    let mut b = DummyBackend::new(Pacing::Freewheel).with_max_cycles(1);
    b.open(BackendConfig {
        auto_connect: true,
        ..cfg(64)
    })
    .unwrap();
    b.start(Box::new(|_| {})).unwrap();
    let mut c: Vec<_> = b.connections().map(|(a, b)| format!("{a}>{b}")).collect();
    c.sort();
    assert_eq!(
        c,
        [
            "system:capture_1>test:audio_in_1",
            "system:midi_capture_1>test:midi_in",
            "test:audio_out_1>system:playback_1",
            "test:audio_out_2>system:playback_2",
        ]
    );
}

#[test]
fn lifecycle_errors_and_restart() {
    let mut b = DummyBackend::new(Pacing::Freewheel);
    assert!(matches!(
        b.start(Box::new(|_| {})),
        Err(BackendError::NotOpen)
    ));
    b.open(cfg(64)).unwrap();
    assert!(matches!(b.open(cfg(64)), Err(BackendError::AlreadyOpen)));
    b.start(Box::new(|_| {})).unwrap();
    assert!(b.is_running());
    assert!(matches!(
        b.start(Box::new(|_| {})),
        Err(BackendError::AlreadyRunning)
    ));
    b.stop().unwrap();
    assert!(!b.is_running());
    let before = b.stats().cycles;
    b.start(Box::new(|_| {})).unwrap();
    assert!(b.wait_for_cycles(before + 10, WAIT));
    b.close().unwrap();
    assert!(!b.is_running());
    assert!(matches!(
        b.list_ports(&PortFilter::default()),
        Err(BackendError::NotOpen)
    ));
    assert!(matches!(
        DummyBackend::default().open(BackendConfig {
            buffer_size: Some(0),
            ..cfg(64)
        }),
        Err(BackendError::Config(_))
    ));
}

#[test]
fn stop_drops_the_callback() {
    struct Flag(std::sync::Arc<std::sync::atomic::AtomicBool>);
    impl Drop for Flag {
        fn drop(&mut self) {
            self.0.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }
    let dropped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = Flag(dropped.clone());
    let mut b = DummyBackend::new(Pacing::Freewheel);
    b.open(cfg(64)).unwrap();
    b.start(Box::new(move |_| {
        let _ = &flag;
    }))
    .unwrap();
    b.stop().unwrap();
    assert!(dropped.load(std::sync::atomic::Ordering::SeqCst));
}

/// Starts `b` with a callback that sends every MIDI input event to the
/// returned channel.
fn start_forwarding_midi_in(b: &mut DummyBackend) -> mpsc::Receiver<Event> {
    let (tx, rx) = mpsc::sync_channel(64);
    b.start(Box::new(move |c| {
        for e in c.midi_in.iter() {
            let _ = tx.try_send(*e);
        }
    }))
    .unwrap();
    rx
}

const CC: [u8; 3] = [0xB0, 7, 99];

#[test]
fn injector_made_while_running_delivers_after_restart() {
    let mut b = DummyBackend::new(Pacing::Freewheel);
    b.open(cfg(32)).unwrap();
    let _first = b.midi_injector();
    b.start(Box::new(|_| {})).unwrap();
    let mut second = b.midi_injector();
    b.stop().unwrap();
    let rx = start_forwarding_midi_in(&mut b);
    assert!(second.send(&CC));
    let e = rx.recv_timeout(WAIT).expect("MIDI from the newer injector");
    assert_eq!(e.kind, EventKind::Midi(CC));
    b.stop().unwrap();
}

#[test]
fn queues_survive_a_panicking_callback() {
    let mut b = DummyBackend::new(Pacing::Freewheel);
    b.open(cfg(32)).unwrap();
    let mut inject = b.midi_injector();
    let (tx, rx) = mpsc::sync_channel(1);
    b.start(Box::new(move |_| {
        let _ = tx.try_send(());
        panic!("callback bug");
    }))
    .unwrap();
    rx.recv_timeout(WAIT).unwrap();
    assert!(matches!(b.stop(), Err(BackendError::Other(_))));
    let rx = start_forwarding_midi_in(&mut b);
    assert!(inject.send(&CC));
    let e = rx.recv_timeout(WAIT).expect("MIDI after the panic");
    assert_eq!(e.kind, EventKind::Midi(CC));
    b.stop().unwrap();
}

#[test]
fn queue_survives_a_run_without_the_midi_port() {
    let mut b = DummyBackend::new(Pacing::Freewheel);
    b.open(BackendConfig {
        midi_input: false,
        ..cfg(32)
    })
    .unwrap();
    let mut inject = b.midi_injector();
    b.start(Box::new(|_| {})).unwrap();
    b.stop().unwrap();
    b.close().unwrap();
    b.open(cfg(32)).unwrap();
    let rx = start_forwarding_midi_in(&mut b);
    assert!(inject.send(&CC));
    let e = rx.recv_timeout(WAIT).expect("MIDI once the port is back");
    assert_eq!(e.kind, EventKind::Midi(CC));
    b.stop().unwrap();
}

#[test]
fn stop_does_not_wait_out_a_long_period() {
    // One period of a million frames at 1 Hz is eleven days.
    let mut b = DummyBackend::new(Pacing::RealTime);
    b.open(BackendConfig {
        sample_rate: Some(1),
        buffer_size: Some(1_000_000),
        ..cfg(32)
    })
    .unwrap();
    b.start(Box::new(|_| {})).unwrap();
    assert!(b.wait_for_cycles(1, WAIT));
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let r = b.stop();
        let _ = tx.send(r.is_ok());
    });
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(5)),
        Ok(true),
        "stop() hung"
    );
}

#[test]
fn loopback_returns_the_outputs_after_the_reported_latency() {
    let latency = Latency {
        input: 100,
        output: 70,
    };
    let mut b = DummyBackend::new(Pacing::Freewheel)
        .with_max_cycles(20)
        .with_loopback(latency);
    b.open(cfg(64)).unwrap();
    // The total is longer than a period, so it is kept as given.
    assert_eq!(b.latency(), latency);
    let (tx, rx) = mpsc::sync_channel(20);
    b.start(Box::new(move |c| {
        // Output a ramp of the frame time; the input should repeat it 170
        // frames later.
        let mut late = Vec::new();
        for (i, s) in c.audio_out[0].iter_mut().enumerate() {
            *s = (c.frame_time + i as u64) as f32;
        }
        for (i, &x) in c.audio_in[0].iter().enumerate() {
            let t = c.frame_time + i as u64;
            let want = t.checked_sub(170).map_or(0.0, |t| t as f32);
            if x != want {
                late.push((t, x));
            }
        }
        let _ = tx.try_send(late);
    }))
    .unwrap();
    assert!(b.wait_for_cycles(20, WAIT));
    b.stop().unwrap();
    for late in rx.try_iter() {
        assert!(late.is_empty(), "{late:?}");
    }

    // A round trip shorter than a period is raised to a period.
    let mut b = DummyBackend::new(Pacing::Freewheel).with_loopback(Latency {
        input: 10,
        output: 10,
    });
    b.open(cfg(64)).unwrap();
    assert_eq!(b.latency().total(), 64);
}
