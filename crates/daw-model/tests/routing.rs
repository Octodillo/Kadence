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

/// Removing a bus cuts the routing into it; undoing puts back the routing and nothing else.
/// Plugin state is stored outside the undo history (on save, say), so an undo that restored
/// the routed tracks whole would take it back to what it was when the bus was removed.
#[test]
fn undoing_a_track_removal_restores_only_the_routing_it_cut() {
    let mut p = Project::new("R");
    let mut a = Track::new(TrackKind::Audio, "A");
    let other = Track::new(TrackKind::Bus, "Other");
    let bus = Track::new(TrackKind::Bus, "Bus");
    let mut device = Device::new(
        "Plugin",
        DeviceSource::Plugin {
            format: daw_core::plugin::PluginFormatKind::Clap,
            id: "x".into(),
        },
    );
    device.state = Some(vec![0].into());
    let device_id = device.id;
    a.devices.push(device);
    // The send to the bus sits between two others, and so does its lane.
    let sends = [Send::new(other.id), Send::new(bus.id), Send::new(other.id)];
    a.automation = sends
        .iter()
        .map(|s| AutomationLane::new(AutomationTarget::SendLevel { send: s.id }))
        .collect();
    a.sends = sends.to_vec();
    a.output = TrackOutput::Bus(bus.id);
    let (a_id, bus_id) = (a.id, bus.id);
    p.tracks = vec![a.into(), other.into(), bus.into()];
    let mut doc = Document::new(p);
    assert_eq!(doc.project().validate(), Ok(()));
    let before = doc.project().track(a_id).unwrap().clone();

    doc.execute(RemoveTrack::new(bus_id)).unwrap();
    let cut = doc.project().track(a_id).unwrap();
    assert_eq!(cut.output, TrackOutput::Master);
    assert_eq!(cut.sends, [sends[0].clone(), sends[2].clone()]);
    assert_eq!(cut.automation.len(), 2);
    assert_eq!(doc.project().validate(), Ok(()));

    let newer: StateBlob = vec![1].into();
    doc.store_plugin_state(device_id, Some(newer.clone()))
        .unwrap();
    doc.undo().unwrap();
    let back = doc.project().track(a_id).unwrap();
    assert_eq!(back.devices[0].state, Some(newer), "the stored state stays");
    assert_eq!(back.output, before.output);
    assert_eq!(back.sends, before.sends);
    assert_eq!(back.automation, before.automation);
    assert_eq!(doc.project().validate(), Ok(()));

    doc.redo().unwrap();
    assert_eq!(doc.project().track(a_id).unwrap().sends.len(), 2);
    doc.undo().unwrap();
    assert_eq!(doc.project().track(a_id).unwrap().sends, before.sends);
}
