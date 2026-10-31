//! The plugin hosting contracts, implemented per format by WU6 (CLAP),
//! WU7 (LV2) and WU8 (VST3).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use std::any::Any;

use crate::event::ParamId;
use crate::node::{Node, ProcessContext};

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
    /// returns `Some` while the plugin is prepared; the default, and a format that cannot,
    /// returns `None`.
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
    fn reset(&mut self) {}
    /// For [`PluginInstance::join`] to take back its own type.
    fn into_any(self: Box<Self>) -> Box<dyn Any + Send>;
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
