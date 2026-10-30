//! The plugin hosting contracts, implemented per format by WU6 (CLAP),
//! WU7 (LV2) and WU8 (VST3).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::event::ParamId;
use crate::node::Node;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PluginFormatKind {
    Clap,
    Lv2,
    Vst3,
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
}

/// One plugin format: knows where its plugins live, how to scan them and how
/// to instantiate one.
pub trait PluginFormat: Send + Sync {
    fn kind(&self) -> PluginFormatKind;
    fn default_search_paths(&self) -> Vec<PathBuf>;
    /// Describes the plugins at `path`. A bundle that holds no plugins gives an empty list; one
    /// that cannot be loaded (a missing library, no entry point, a factory that fails) gives
    /// [`PluginError::Load`] saying why, so the host can show the user.
    fn scan(&self, path: &Path) -> Result<Vec<PluginDescriptor>, PluginError>;
    fn instantiate(
        &self,
        d: &PluginDescriptor,
        sample_rate: f64,
        max_frames: u32,
    ) -> Result<Box<dyn PluginInstance>, PluginError>;
}
