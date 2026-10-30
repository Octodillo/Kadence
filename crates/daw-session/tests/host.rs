//! The audio host's MIDI inputs on the dummy backend: a port per chosen
//! source, connected whenever the source is there.

use daw_backend::{DummyBackend, Pacing, PortDirection, PortKind};
use daw_session::host::{AudioHost, HostConfig};

fn host() -> (AudioHost, daw_backend::DummySystem) {
    let dummy = DummyBackend::new(Pacing::RealTime);
    let system = dummy.system();
    system.add_port("keys:out", PortKind::Midi, PortDirection::Output, true);
    system.add_port("seq:out", PortKind::Midi, PortDirection::Output, false);
    let config = HostConfig {
        use_server: false,
        ..Default::default()
    };
    (
        AudioHost::with_backend(Box::new(dummy), &config).unwrap(),
        system,
    )
}

fn linked(system: &daw_backend::DummySystem, from: &str, to: &str) -> bool {
    system
        .connections()
        .iter()
        .any(|(a, b)| a == from && b == to)
}

#[test]
fn lists_midi_sources_of_other_clients() {
    let (host, _) = host();
    let names: Vec<_> = host
        .midi_sources()
        .into_iter()
        .map(|s| (s.name, s.is_physical))
        .collect();
    assert!(names.contains(&("keys:out".into(), true)), "{names:?}");
    assert!(names.contains(&("seq:out".into(), false)), "{names:?}");
    assert!(names.contains(&("system:midi_capture_1".into(), true)));
    assert!(names.iter().all(|(n, _)| !n.starts_with("Kadence:")));
}

#[test]
fn audio_is_connected_and_midi_only_where_chosen() {
    let (host, system) = host();
    assert!(linked(&system, "Kadence:audio_out_1", "system:playback_1"));
    assert!(linked(&system, "system:capture_2", "Kadence:audio_in_2"));
    // Nothing is chosen yet, so no MIDI source is connected anywhere.
    assert!(
        system
            .connections()
            .iter()
            .all(|(_, b)| !b.contains("midi")),
        "{:?}",
        system.connections()
    );
    drop(host);
}

#[test]
fn a_chosen_source_gets_its_own_port_and_comes_back_after_unplugging() {
    let (mut host, system) = host();
    let failed = host.set_midi_inputs(&["keys:out".into(), "seq:out".into()]);
    assert!(failed.is_empty(), "{failed:?}");
    assert!(linked(&system, "keys:out", "Kadence:out"));
    assert!(linked(&system, "seq:out", "Kadence:out 2"));
    assert!(!linked(&system, "keys:out", "Kadence:midi_in"));
    let sources = host.session().midi_sources().clone();
    assert_eq!(sources.len(), 2);
    assert_ne!(sources["keys:out"], sources["seq:out"]);
    assert!(sources.values().all(|&i| i > 0), "source 0 is midi_in");
    assert!(host.session().wants_sync());

    // Unplugged: the port stays, so the track keeps its source.
    system.remove_port("keys:out");
    assert!(host.refresh_midi());
    assert_eq!(
        host.midi_inputs(),
        [("keys:out".into(), false), ("seq:out".into(), true)]
    );
    assert!(!host.refresh_midi(), "nothing changed since");
    // Plugged back in: connected again.
    system.add_port("keys:out", PortKind::Midi, PortDirection::Output, true);
    assert!(host.refresh_midi());
    assert!(linked(&system, "keys:out", "Kadence:out"));
    assert_eq!(host.session().midi_sources(), &sources);

    // Dropping a source drops its port.
    host.set_midi_inputs(&["seq:out".into()]);
    assert!(!linked(&system, "keys:out", "Kadence:out"));
    assert_eq!(host.backend().midi_inputs().len(), 1);
    assert_eq!(host.session().midi_sources().len(), 1);
}

#[test]
fn a_source_that_is_not_there_yet_is_connected_when_it_appears() {
    let (mut host, system) = host();
    assert!(host.set_midi_inputs(&["pads:out".into()]).is_empty());
    assert_eq!(host.midi_inputs(), [("pads:out".into(), false)]);
    system.add_port("pads:out", PortKind::Midi, PortDirection::Output, true);
    assert!(host.refresh_midi());
    assert!(linked(&system, "pads:out", "Kadence:out"));
}
