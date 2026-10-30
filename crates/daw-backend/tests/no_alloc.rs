//! The backend's own per-cycle work must not allocate. With `RtAllocCounter`
//! installed, the backend counts allocations made inside a cycle.

use std::time::Duration;

use daw_backend::{AudioBackend, BackendConfig, DummyBackend, Pacing, RtAllocCounter};
use daw_core::event::{Event, EventKind};

#[global_allocator]
static ALLOC: RtAllocCounter = RtAllocCounter;

#[test]
#[cfg(debug_assertions)]
fn dummy_cycles_do_not_allocate() {
    let mut b = DummyBackend::new(Pacing::Freewheel).with_max_cycles(2_000);
    b.open(BackendConfig {
        midi_output: true,
        auto_connect: false,
        ..BackendConfig::default()
    })
    .unwrap();
    let mut inject = b.midi_injector();
    let mut tap = b.midi_tap();
    for k in 0..100 {
        inject.send(&[0x90, k, 64]);
        inject.send(&[0x80, k, 0]);
    }
    let mut phase = 0f32;
    b.start(Box::new(move |c| {
        let notes = c
            .midi_in
            .iter()
            .filter(|e| matches!(e.kind, EventKind::NoteOn { .. }));
        let n = notes.count();
        for out in c.audio_out.iter_mut() {
            for s in out.iter_mut() {
                phase = (phase + 0.01).fract();
                *s = phase;
            }
        }
        if n > 0 {
            let _ = c.midi_out.push(Event {
                frame: 1,
                kind: EventKind::Midi([0xB0, 1, 2]),
            });
        }
    }))
    .unwrap();
    assert!(b.wait_for_cycles(2_000, Duration::from_secs(30)));
    b.stop().unwrap();
    assert!(tap.try_recv().is_some());
    assert_eq!(b.stats().rt_alloc_violations, 0, "a cycle allocated");
}

/// Set in the child process `allocating_cycles_are_counted_silently` runs.
const CHILD: &str = "DAW_BACKEND_ALLOC_CHILD";

/// A callback that allocates every cycle is counted, and nothing is written
/// to stderr from the audio thread. The cycles run in a child process (this
/// test binary again) so its stderr can be checked.
#[test]
#[cfg(debug_assertions)]
fn allocating_cycles_are_counted_silently() {
    const CYCLES: u64 = 200;
    if std::env::var_os(CHILD).is_some() {
        let mut b = DummyBackend::new(Pacing::Freewheel).with_max_cycles(CYCLES);
        b.open(BackendConfig {
            auto_connect: false,
            ..BackendConfig::default()
        })
        .unwrap();
        b.start(Box::new(|_| {
            std::hint::black_box(vec![0u8; 64]);
        }))
        .unwrap();
        assert!(b.wait_for_cycles(CYCLES, Duration::from_secs(30)));
        b.stop().unwrap();
        // One allocation and one deallocation per cycle.
        assert_eq!(b.stats().rt_alloc_violations, 2 * CYCLES);
        return;
    }
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "allocating_cycles_are_counted_silently",
            "--nocapture",
        ])
        .env(CHILD, "1")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "child failed: {stderr}");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("1 passed"),
        "child did not run the test"
    );
    assert!(stderr.trim().is_empty(), "wrote to stderr: {stderr}");
}
