//! Undo of merged edit groups restores the project exactly.

use daw_arrange::commands::{Group, PlaceClip, TrimClip};
use daw_core::time::{PPQ, SampleTime, TickTime};
use daw_model::commands::{AddClip, AddTrack};
use daw_model::{AudioClip, Clip, ClipContent, Command, Document, Project, Track, TrackKind};

fn aclip(start: i64) -> Clip {
    Clip::new(
        "a",
        TickTime(start),
        TickTime(PPQ),
        ClipContent::Audio(AudioClip {
            source: "x.wav".into(),
            offset: SampleTime(0),
            gain_db: 0.0,
        }),
    )
}

#[test]
fn merged_group_move_undo_restores_order() {
    let mut doc = Document::new(Project::new("t"));
    let t = Track::new(TrackKind::Audio, "T");
    let tid = t.id;
    doc.execute(AddTrack::new(0, t)).unwrap();
    // clips A@0, X@10PPQ, B@20PPQ ; move A and B together
    let a = aclip(0);
    let x = aclip(10 * PPQ);
    let b = aclip(20 * PPQ);
    let (ia, ix, ib) = (a.id, x.id, b.id);
    for c in [a, x, b] {
        doc.execute(AddClip::new(tid, c)).unwrap();
    }
    doc.seal();
    let before: Vec<_> = doc.project().tracks[0]
        .clips
        .iter()
        .map(|c| (c.id, c.start))
        .collect();
    let mk = |d: i64| -> Group {
        let items: Vec<Box<dyn Command>> = vec![
            Box::new(PlaceClip::new(ia, tid, TickTime(d))),
            Box::new(PlaceClip::new(ib, tid, TickTime(20 * PPQ + d))),
        ];
        Group::new("Move Clips", vec![ia, ib], items)
    };
    doc.execute(mk(5 * PPQ)).unwrap(); // A@5, X@10, B@25
    doc.execute(mk(15 * PPQ)).unwrap(); // A@15 (after X), B@35 -> merges
    doc.seal();
    doc.undo().unwrap();
    let after: Vec<_> = doc.project().tracks[0]
        .clips
        .iter()
        .map(|c| (c.id, c.start))
        .collect();
    let _ = ix;
    assert_eq!(before, after, "order after undo");
}

#[test]
fn trim_merge_redo() {
    let mut doc = Document::new(Project::new("t"));
    let t = Track::new(TrackKind::Audio, "T");
    let tid = t.id;
    doc.execute(AddTrack::new(0, t)).unwrap();
    let a = aclip(4 * PPQ);
    let ia = a.id;
    doc.execute(AddClip::new(tid, a)).unwrap();
    doc.seal();
    doc.execute(TrimClip::new(
        ia,
        TickTime(5 * PPQ),
        TickTime(3 * PPQ),
        Some(SampleTime(100)),
    ))
    .unwrap();
    doc.execute(TrimClip::new(
        ia,
        TickTime(6 * PPQ),
        TickTime(2 * PPQ),
        Some(SampleTime(200)),
    ))
    .unwrap();
    doc.seal();
    doc.undo().unwrap();
    let c = &doc.project().tracks[0].clips[0];
    assert_eq!((c.start, c.length), (TickTime(4 * PPQ), TickTime(PPQ)));
    doc.redo().unwrap();
    let c = &doc.project().tracks[0].clips[0];
    assert_eq!((c.start, c.length), (TickTime(6 * PPQ), TickTime(2 * PPQ)));
}

#[test]
fn merged_multi_clip_moves_undo_exactly() {
    let mut seed: u64 = 12345;
    let mut rnd = |n: u64| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed % n
    };
    for iter in 0..3000 {
        let mut doc = Document::new(Project::new("t"));
        let mut tids = vec![];
        for i in 0..2 {
            let t = Track::new(TrackKind::Audio, "T");
            tids.push(t.id);
            doc.execute(AddTrack::new(i, t)).unwrap();
        }
        let n = 2 + rnd(5) as usize;
        let mut clips = vec![];
        for _ in 0..n {
            let tr = rnd(2) as usize;
            let c = aclip(rnd(20) as i64 * PPQ);
            clips.push((c.id, tr, c.start));
            doc.execute(AddClip::new(tids[tr], c)).unwrap();
        }
        doc.seal();
        // Each track's clips, which must stay sorted by start. The order of
        // clips with the same start isn't part of the model.
        let snap = |d: &Document| -> Vec<Vec<(TickTime, daw_model::ClipId)>> {
            d.project()
                .tracks
                .iter()
                .map(|t| {
                    let mut v: Vec<_> = t.clips.iter().map(|c| (c.start, c.id)).collect();
                    assert!(v.is_sorted_by_key(|c| c.0), "clips out of order");
                    v.sort();
                    v
                })
                .collect()
        };
        let before = snap(&doc);
        let sel: Vec<_> = clips.iter().filter(|_| rnd(2) == 0).cloned().collect();
        if sel.is_empty() {
            continue;
        }
        let steps = 1 + rnd(4);
        for _ in 0..steps {
            let d = rnd(30) as i64 * PPQ;
            let drow = rnd(2) as usize;
            let items: Vec<Box<dyn Command>> = sel
                .iter()
                .map(|&(id, r, s)| {
                    Box::new(PlaceClip::new(id, tids[(r + drow) % 2], TickTime(s.0 + d)))
                        as Box<dyn Command>
                })
                .collect();
            let _ = doc.execute(Group::new(
                "Move Clips",
                sel.iter().map(|c| c.0).collect(),
                items,
            ));
        }
        doc.seal();
        doc.undo().unwrap();
        let after = snap(&doc);
        assert_eq!(before, after, "iter {iter}");
    }
}
