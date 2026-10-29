//! The backend's own per-cycle work must not allocate. With `AllocDisabler`
//! installed, the backend counts allocations made inside a cycle.

use std::time::Duration;

use assert_no_alloc::AllocDisabler;
use daw_backend::{AudioBackend, BackendConfig, DummyBackend, Pacing};
use daw_core::event::{Event, EventKind};

#[global_allocator]
static ALLOC: AllocDisabler = AllocDisabler;

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
