//! The plugin hosting contracts, implemented per format by `daw-plugins-clap`,
//! `daw-plugins-lv2` and `daw-plugins-vst3`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use std::any::Any;

use crate::event::ParamId;
use crate::node::{Node, ProcessContext};

mod search;

pub use search::default_search_paths;

/// The name every format gives plugins that ask which host they run in.
pub const HOST_NAME: &str = "Kadence";

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PluginFormatKind {
    Clap,
    Lv2,
    Vst3,
}

impl PluginFormatKind {
    /// The format's name wherever one is written down or passed along: the
    /// scanner's command line, QML's `formatId`, and the extension of the
    /// format's bundles.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Clap => "clap",
            Self::Lv2 => "lv2",
            Self::Vst3 => "vst3",
        }
    }

    /// The format's name as users know it. Not translated: it is a name.
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Clap => "CLAP",
            Self::Lv2 => "LV2",
            Self::Vst3 => "VST3",
        }
    }
}

/// Not the [`as_str`](PluginFormatKind::as_str) of any format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnknownPluginFormat;

impl std::str::FromStr for PluginFormatKind {
    type Err = UnknownPluginFormat;

    /// The format whose [`as_str`](Self::as_str) is `name`.
    fn from_str(name: &str) -> Result<Self, Self::Err> {
        [Self::Clap, Self::Lv2, Self::Vst3]
            .into_iter()
            .find(|kind| kind.as_str() == name)
            .ok_or(UnknownPluginFormat)
    }
}

/// What a scan finds out about one plugin.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PluginDescriptor {
    pub format: PluginFormatKind,
    /// Format-specific unique id (CLAP id, LV2 URI, VST3 class id).
    pub id: String,
    pub name: String,
    pub vendor: String,
    /// The bundle or library the plugin was found in.
    pub path: PathBuf,
    pub is_instrument: bool,
    /// Main audio (inputs, outputs) channel counts.
    pub ports: (u16, u16),
}

/// What a scan of one bundle found.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct BundleScan {
    pub plugins: Vec<PluginDescriptor>,
    /// Why each plugin the bundle holds but could not describe was skipped: a sentence that
    /// names the plugin (in English, often with the plugin's own words). The host lists them
    /// as failures and scans the bundle again when asked to retry.
    #[serde(default)]
    pub skipped: Vec<String>,
}

impl BundleScan {
    /// A scan that described every plugin it found.
    pub fn complete(plugins: Vec<PluginDescriptor>) -> Self {
        Self {
            plugins,
            skipped: Vec::new(),
        }
    }
}

/// Builds a [`BundleScan`] one plugin at a time, for a format that has to load each plugin to
/// describe it. Plugins that fail are skipped; a scan in which every plugin failed is itself a
/// failure, with the first plugin's error.
#[derive(Debug, Default)]
pub struct ScanBuilder {
    scan: BundleScan,
    first_error: Option<PluginError>,
}

impl ScanBuilder {
    pub fn found(&mut self, plugin: PluginDescriptor) {
        self.scan.plugins.push(plugin);
    }

    /// Records a plugin that could not be described. `error` should name the plugin.
    pub fn skip(&mut self, error: PluginError) {
        self.scan.skipped.push(error.reason());
        self.first_error.get_or_insert(error);
    }

    /// Adds the outcome of scanning one bundle of several under the scanned path; a bundle
    /// that failed counts as skipped.
    pub fn bundle(&mut self, scanned: Result<BundleScan, PluginError>) {
        match scanned {
            Ok(scan) => {
                self.scan.plugins.extend(scan.plugins);
                self.scan.skipped.extend(scan.skipped);
            }
            Err(e) => self.skip(e),
        }
    }

    pub fn finish(self) -> Result<BundleScan, PluginError> {
        match self.first_error {
            Some(e) if self.scan.plugins.is_empty() => Err(e),
            _ => Ok(self.scan),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ParamInfo {
    pub id: ParamId,
    pub name: String,
    pub min: f64,
    pub max: f64,
    pub default: f64,
    pub automatable: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    #[error("load: {0}")]
    Load(String),
    #[error("state: {0}")]
    State(String),
    #[error("editor: {0}")]
    Editor(String),
    /// The editor needs an X11 display (XWayland under Wayland) and none could be reached.
    #[error("no X11 display: {0}")]
    NoDisplay(String),
    /// The plugin could not take the call now, but has not failed: a sandboxed plugin whose
    /// editor shows a modal dialog, say. The call can be made again later.
    #[error("busy: {0}")]
    Busy(String),
}

impl PluginError {
    /// What went wrong, for a list that already says which step failed: a load failure's own
    /// text, without the `load:` in front.
    pub fn reason(&self) -> String {
        match self {
            Self::Load(why) => why.clone(),
            other => other.to_string(),
        }
    }
}

/// A loaded, activated plugin. It is a graph [`Node`]; the other methods run
/// off the audio thread.
pub trait PluginInstance: Node {
    fn descriptor(&self) -> &PluginDescriptor;
    fn params(&self) -> Vec<ParamInfo>;
    /// Called off the real-time thread.
    fn param_value(&self, id: ParamId) -> f64;
    fn save_state(&mut self) -> Result<Vec<u8>, PluginError>;
    fn load_state(&mut self, data: &[u8]) -> Result<(), PluginError>;
    fn has_editor(&self) -> bool;
    /// Opens the editor as a floating window.
    fn open_editor(&mut self) -> Result<(), PluginError>;
    fn close_editor(&mut self);
    /// Whether the editor is open. Turns false once the user closes the window, as noticed by
    /// [`on_main_thread`](Self::on_main_thread). The default never reports one open.
    fn is_editor_open(&self) -> bool {
        false
    }
    /// The title of the editor's window, from now on: an open window takes it at once, and a
    /// later [`open_editor`](Self::open_editor) uses it. Until set, the window shows the
    /// plugin's name. The default ignores it.
    fn set_editor_title(&mut self, title: &str) {
        let _ = title;
    }
    /// Returns true, once, after the plugin said its state changed on its own (a preset
    /// loaded or a control moved in its editor), as noticed by
    /// [`on_main_thread`](Self::on_main_thread). The host can then save the state and record
    /// it as an edit. [`save_state`](Self::save_state) does not clear it. The default never
    /// reports a change.
    fn take_state_changed(&mut self) -> bool {
        false
    }
    /// Host idle / main-thread callback pump.
    fn on_main_thread(&mut self) {}
    /// Returns true, once, after the plugin asked for another port layout or latency, as
    /// noticed by [`on_main_thread`](Self::on_main_thread). The host should then recompile
    /// the graph that holds the instance, which prepares it again and picks up the new
    /// `ports` and `latency`.
    ///
    /// Formats that implement this keep `ports` and `latency` unchanged until that
    /// [`Node::prepare`]; a plugin that cannot go on with its old layout outputs silence
    /// until then. The default never reports a change.
    fn take_layout_changed(&mut self) -> bool {
        false
    }
    /// Hands out the audio-thread half of the plugin, so the audio thread can run `process`
    /// while this object serves main-thread calls (`on_main_thread`, state, editors). Without
    /// it the host has to keep the two apart, and a slow main-thread call costs the plugin's
    /// audio. A format whose plugins allow those calls during processing (CLAP, VST3)
    /// returns `Some` while the plugin is prepared, and one whose host side keeps the
    /// plugin's UI away from its audio (LV2) at any time; the default, and a format that
    /// cannot, returns `None`.
    ///
    /// While the half is out, this object's own [`Node::process`] and [`Node::reset`] do
    /// nothing, and the host gives the half back with [`join`](Self::join) before it calls
    /// [`Node::prepare`] or drops the instance. `ports` and `latency` stay those of the
    /// prepare that came before.
    fn split(&mut self) -> Option<Box<dyn PluginProcessor>> {
        None
    }
    /// Takes back the half [`split`](Self::split) handed out, once the audio thread no longer
    /// uses it. Called on the main thread.
    fn join(&mut self, processor: Box<dyn PluginProcessor>) {
        drop(processor);
    }
}

/// The audio-thread half of a plugin, from [`PluginInstance::split`]. Real-time safe, like
/// [`Node::process`].
pub trait PluginProcessor: Send {
    fn process(&mut self, ctx: &mut ProcessContext);
    /// Clears the plugin's internal state, at the latest at the start of the next
    /// [`process`](Self::process): a half that cannot reach the plugin now may defer it.
    fn reset(&mut self) {}
    /// For [`PluginInstance::join`] to take back its own type.
    fn into_any(self: Box<Self>) -> Box<dyn Any + Send>;
}

/// One plugin format: knows where its plugins live, how to scan them and how
/// to instantiate one.
pub trait PluginFormat: Send + Sync {
    fn kind(&self) -> PluginFormatKind;
    /// Where this format's bundles are installed: the standard locations of its kind (see
    /// [`default_search_paths`]) unless the format has its own.
    fn default_search_paths(&self) -> Vec<PathBuf> {
        default_search_paths(&self.kind())
    }
    /// Describes the plugins at `path`. A bundle that holds no plugins gives an empty list; one
    /// that cannot be loaded (a missing library, no entry point, a factory that fails) gives
    /// [`PluginError::Load`] saying why, so the host can show the user. When only some of a
    /// bundle's plugins can be described, the rest are named in [`BundleScan::skipped`].
    fn scan(&self, path: &Path) -> Result<BundleScan, PluginError>;
    /// Called once before the process exits, after every instance of this format is gone.
    /// A format that keeps plugin code loaded (VST3) lets the plugins clean up here.
    fn shutdown(&self) {}
    fn instantiate(
        &self,
        d: &PluginDescriptor,
        sample_rate: f64,
        max_frames: u32,
    ) -> Result<Box<dyn PluginInstance>, PluginError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_format_is_found_again_by_its_name() {
        for kind in [
            PluginFormatKind::Clap,
            PluginFormatKind::Lv2,
            PluginFormatKind::Vst3,
        ] {
            assert_eq!(kind.as_str().parse(), Ok(kind.clone()));
            // The display name is for people, not a name to parse.
            assert_eq!(
                kind.display_name().parse::<PluginFormatKind>(),
                Err(UnknownPluginFormat)
            );
        }
        assert_eq!("".parse::<PluginFormatKind>(), Err(UnknownPluginFormat));
    }

    fn plugin(id: &str) -> PluginDescriptor {
        PluginDescriptor {
            format: PluginFormatKind::Clap,
            id: id.into(),
            name: id.into(),
            vendor: String::new(),
            path: PathBuf::from("/x.clap"),
            is_instrument: false,
            ports: (2, 2),
        }
    }

    fn load_error(why: &str) -> PluginError {
        PluginError::Load(why.into())
    }

    #[test]
    fn a_scan_skips_plugins_that_fail_and_fails_when_all_do() {
        let mut some = ScanBuilder::default();
        some.skip(load_error("b: no licence"));
        some.found(plugin("a"));
        some.skip(PluginError::State("c: bad state".into()));
        let scan = some.finish().expect("one plugin was described");
        assert_eq!(scan.plugins.len(), 1);
        assert_eq!(scan.skipped, ["b: no licence", "state: c: bad state"]);

        let mut none = ScanBuilder::default();
        none.skip(load_error("b: no licence"));
        none.skip(load_error("c: no display"));
        match none.finish() {
            Err(PluginError::Load(why)) => assert_eq!(why, "b: no licence"),
            other => panic!("expected the first error, got {other:?}"),
        }

        let empty = ScanBuilder::default()
            .finish()
            .expect("no plugins is no failure");
        assert!(empty.plugins.is_empty() && empty.skipped.is_empty());
    }

    #[test]
    fn bundles_under_one_path_add_up() {
        let mut all = ScanBuilder::default();
        all.bundle(Ok(BundleScan {
            plugins: vec![plugin("a")],
            skipped: vec!["b: no licence".into()],
        }));
        all.bundle(Err(load_error("/Broken.vst3: not a library")));
        all.bundle(Ok(BundleScan::complete(vec![plugin("c")])));
        let scan = all.finish().unwrap();
        assert_eq!(scan.plugins.len(), 2);
        assert_eq!(
            scan.skipped,
            ["b: no licence", "/Broken.vst3: not a library"]
        );
    }
}
