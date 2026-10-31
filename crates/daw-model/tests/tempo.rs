//! Tempo and time signature edits: the helpers the transport bar uses and
//! their undo.

use daw_core::time::{PPQ, TickTime};
use daw_model::commands::{SetTempo, SetTimeSignature};
use daw_model::{Document, MeterPoint, Project, TempoData, TempoPoint};

fn two_tempos() -> TempoData {
    let mut t = TempoData::default();
    t.tempos.push(TempoPoint {
        tick: TickTime(8 * PPQ),
        bpm: 90.0,
        ramp: false,
    });
    t.meters.push(MeterPoint {
        tick: TickTime(8 * PPQ),
        numerator: 3,
        denominator: 4,
    });
    t
}

#[test]
fn reads_and_edits_the_point_in_effect() {
    let t = two_tempos();
    assert_eq!(t.bpm_at(TickTime(0)), 120.0);
    assert_eq!(t.bpm_at(TickTime(8 * PPQ - 1)), 120.0);
    assert_eq!(t.bpm_at(TickTime(20 * PPQ)), 90.0);
    assert_eq!(t.meter_at(TickTime(9 * PPQ)), (3, 4));

    let faster = t.with_bpm_at(TickTime(10 * PPQ), 100.0);
    assert_eq!(faster.tempos[0].bpm, 120.0);
    assert_eq!(faster.tempos[1].bpm, 100.0);
    let meter = t.with_meter_at(TickTime(PPQ), 6, 8);
    assert_eq!(meter.meter_at(TickTime(0)), (6, 8));
    assert_eq!(meter.meter_at(TickTime(8 * PPQ)), (3, 4));
    meter.validate().unwrap();
}

#[test]
fn edits_are_clamped_to_valid_values() {
    let t = TempoData::default();
    assert_eq!(t.with_bpm_at(TickTime(0), 5.0).bpm_at(TickTime(0)), 20.0);
    assert_eq!(
        t.with_bpm_at(TickTime(0), 5000.0).bpm_at(TickTime(0)),
        999.0
    );
    assert_eq!(
        t.with_bpm_at(TickTime(0), f64::NAN).bpm_at(TickTime(0)),
        120.0
    );
    for (den, want) in [(0, 1), (1, 1), (3, 2), (5, 4), (8, 8), (12, 8), (200, 32)] {
        let m = t.with_meter_at(TickTime(0), 4, den);
        assert_eq!(m.meter_at(TickTime(0)).1, want, "denominator {den}");
        m.validate().unwrap();
    }
    assert_eq!(
        t.with_meter_at(TickTime(0), 0, 4).meter_at(TickTime(0)).0,
        1
    );
    assert_eq!(
        t.with_meter_at(TickTime(0), 99, 4).meter_at(TickTime(0)).0,
        32
    );
}

#[test]
fn tempo_drags_merge_into_one_undo_step_and_undo_restores() {
    let mut doc = Document::new(Project::new("t"));
    for bpm in [121.0, 125.0, 130.0] {
        let tempo = doc.project().tempo.with_bpm_at(TickTime(0), bpm);
        doc.execute(SetTempo { tempo }).unwrap();
    }
    doc.seal();
    let tempo = doc.project().tempo.with_meter_at(TickTime(0), 7, 8);
    doc.execute(SetTimeSignature { tempo }).unwrap();
    assert_eq!(
        doc.history().undo_label().as_deref(),
        Some("Change Time Signature")
    );
    assert_eq!(doc.project().tempo.bpm_at(TickTime(0)), 130.0);
    assert_eq!(doc.project().tempo.meter_at(TickTime(0)), (7, 8));

    doc.undo().unwrap();
    assert_eq!(doc.project().tempo.meter_at(TickTime(0)), (4, 4));
    assert_eq!(doc.project().tempo.bpm_at(TickTime(0)), 130.0);
    doc.undo().unwrap();
    assert_eq!(doc.project().tempo, TempoData::default());
    assert!(!doc.history().can_undo());
    doc.redo().unwrap();
    assert_eq!(doc.project().tempo.bpm_at(TickTime(0)), 130.0);
}
