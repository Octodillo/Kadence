//! Connecting our ports to the hardware, the same way on every backend.

use crate::{AudioBackend, PortDirection, PortFilter, PortInfo, PortKind};

/// The port part of the main MIDI input's name
/// ([`BackendConfig::midi_input`](crate::BackendConfig::midi_input)).
pub const MAIN_MIDI_INPUT: &str = "midi_in";
/// The port part of the MIDI output's name
/// ([`BackendConfig::midi_output`](crate::BackendConfig::midi_output)).
pub const MAIN_MIDI_OUTPUT: &str = "midi_out";

/// Connects our ports to the system's physical ones: our audio outputs to the
/// playback ports and the capture ports to our audio inputs, each in order,
/// so the speakers and the microphone. With `midi`, also every hardware MIDI
/// source to our main MIDI input.
///
/// Call it once after [`open`](AudioBackend::open), before or after
/// [`start`](AudioBackend::start); a backend keeps the links over a stop and
/// start, so calling it again would bring back what the user has unplugged
/// since. Pairs that are already connected (a session manager may link us
/// first) or cannot be are skipped.
///
/// Our ports pair up in the order the backend lists them, which is the order
/// they were registered in: `audio_out_1` first.
pub fn connect_physical(backend: &mut dyn AudioBackend, midi: bool) {
    let list = |kind, direction, ours: bool| -> Vec<PortInfo> {
        let filter = PortFilter {
            kind: Some(kind),
            direction: Some(direction),
            physical_only: !ours,
        };
        backend
            .list_ports(&filter)
            .unwrap_or_default()
            .into_iter()
            .filter(|p| p.is_ours == ours)
            .collect()
    };
    use PortDirection::{Input, Output};
    use PortKind::{Audio, Midi};
    let mut pairs: Vec<(PortInfo, PortInfo)> = list(Audio, Output, true)
        .into_iter()
        .zip(list(Audio, Input, false))
        .chain(
            list(Audio, Output, false)
                .into_iter()
                .zip(list(Audio, Input, true)),
        )
        .collect();
    if midi {
        // The main input alone: a port from `add_midi_input` is the
        // caller's to connect, whatever it is called.
        let added = backend.midi_inputs();
        let main = list(Midi, Input, true).into_iter().find(|p| {
            p.name.split_once(':').map(|(_, port)| port) == Some(MAIN_MIDI_INPUT)
                && !added.iter().any(|a| a.name == p.name)
        });
        if let Some(main) = main {
            pairs.extend(
                list(Midi, Output, false)
                    .into_iter()
                    .map(|source| (source, main.clone())),
            );
        }
    }
    for (from, to) in pairs {
        if let Err(e) = backend.connect(&from.name, &to.name) {
            log::debug!("auto-connect {} -> {}: {e}", from.name, to.name);
        }
    }
}
