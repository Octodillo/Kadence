//! A plugin's audio ports, read the same way for the scan and for a running instance.

use clack_extensions::audio_ports::{AudioPortFlags, AudioPortInfoBuffer, PluginAudioPorts};
use clack_host::prelude::PluginMainThreadHandle;

/// The audio ports of one direction.
#[derive(Clone, Debug, Default)]
pub(crate) struct PortSet {
    /// Channel count of every port, in the plugin's order. A port the plugin fails to describe
    /// has none.
    pub(crate) channels: Vec<usize>,
    /// The port the graph connects to; the others (sidechains, extra outputs) get silence and
    /// are discarded.
    pub(crate) main: Option<usize>,
}

impl PortSet {
    /// Asks the plugin for its input or output ports. Main thread, plugin inactive.
    pub(crate) fn read(
        ports: PluginAudioPorts,
        handle: &PluginMainThreadHandle,
        is_input: bool,
        is_instrument: bool,
    ) -> Self {
        let mut buf = AudioPortInfoBuffer::new();
        let infos: Vec<(AudioPortFlags, u32)> = (0..ports.count(handle, is_input))
            .map(|i| {
                ports
                    .get(handle, i, is_input, &mut buf)
                    .map_or((AudioPortFlags::empty(), 0), |p| (p.flags, p.channel_count))
            })
            .collect();
        Self {
            channels: infos.iter().map(|&(_, c)| c as usize).collect(),
            main: main_port(&infos, is_input, is_instrument),
        }
    }

    /// Channel count of the main port, 0 without one.
    pub(crate) fn main_channels(&self) -> u16 {
        self.main
            .and_then(|i| self.channels.get(i))
            .map_or(0, |&c| u16::try_from(c).unwrap_or(u16::MAX))
    }
}

/// Which port the graph connects to: the one flagged main, else the first. Many effects leave
/// the main flag off, but an instrument's unflagged input is a sidechain (Surge XT's is), not the
/// signal to process.
fn main_port(
    ports: &[(AudioPortFlags, u32)],
    is_input: bool,
    is_instrument: bool,
) -> Option<usize> {
    ports
        .iter()
        .position(|(flags, _)| flags.contains(AudioPortFlags::IS_MAIN))
        .or_else(|| (!ports.is_empty() && !(is_input && is_instrument)).then_some(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unflagged_ports_count_except_an_instruments_inputs() {
        let main = AudioPortFlags::IS_MAIN;
        let none = AudioPortFlags::empty();
        assert_eq!(main_port(&[(none, 2), (main, 2)], true, true), Some(1));
        assert_eq!(main_port(&[(none, 2)], true, true), None);
        assert_eq!(main_port(&[(none, 2)], true, false), Some(0));
        assert_eq!(main_port(&[(none, 2), (none, 1)], false, true), Some(0));
        assert_eq!(main_port(&[], false, false), None);
    }
}
