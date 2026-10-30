//! Outputs and sends between tracks: what reaches what, and loop refusal.

use daw_model::commands::*;
use daw_model::*;

#[test]
fn downstream_follows_outputs_and_sends() {
    let mut p = Project::new("R");
    let a = Track::new(TrackKind::Audio, "A");
    let group = Track::new(TrackKind::Bus, "Group");
    let fx = Track::new(TrackKind::Bus, "FX");
    let (a_id, group_id, fx_id, master) = (a.id, group.id, fx.id, p.master.id);
    p.tracks = vec![a.into(), group.into(), fx.into()];
    let mut doc = Document::new(p);
    doc.execute(SetTrack::new(
        a_id,
        TrackField::Output(TrackOutput::Bus(group_id)),
    ))
    .unwrap();
    doc.execute(AddSend {
        track: group_id,
        send: Send::new(fx_id),
    })
    .unwrap();
    let p = doc.project();
    let reach = |t| {
        let mut v: Vec<_> = p.downstream(t).into_iter().collect();
        v.sort_by_key(|t: &TrackId| t.0);
        v
    };
    let mut all = vec![group_id, fx_id, master];
    all.sort_by_key(|t| t.0);
    assert_eq!(reach(a_id), all);
    assert_eq!(reach(fx_id), [master]);
    assert!(p.downstream(master).is_empty());
    assert!(p.feeds(a_id, fx_id) && !p.feeds(fx_id, a_id));
    assert!(p.feeds(fx_id, fx_id), "a track feeds itself");

    // FX can't route back into the group, as output or send.
    assert!(
        doc.execute(SetTrack::new(
            fx_id,
            TrackField::Output(TrackOutput::Bus(group_id))
        ))
        .is_err()
    );
    assert!(
        doc.execute(AddSend {
            track: fx_id,
            send: Send::new(group_id),
        })
        .is_err()
    );
    assert_eq!(doc.project().validate(), Ok(()));
}
