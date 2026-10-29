//! Plugging the sandbox into the plugin registry.

use std::path::{Path, PathBuf};

use daw_core::plugin::{
    PluginDescriptor, PluginError, PluginFormat, PluginFormatKind, PluginInstance,
};
use daw_plugins::PluginRegistry;

use crate::client::{SandboxConfig, SandboxedInstance};

/// A format whose instances run in a host process. Scanning also runs in a child process.
pub struct SandboxFormat {
    kind: PluginFormatKind,
    search_paths: Vec<PathBuf>,
    config: SandboxConfig,
}

impl SandboxFormat {
    /// Sandboxes `kind`, searching `search_paths` by default. The host program must support
    /// `kind`.
    pub fn new(kind: PluginFormatKind, search_paths: Vec<PathBuf>, config: SandboxConfig) -> Self {
        Self {
            kind,
            search_paths,
            config,
        }
    }

    /// Sandboxes the same kind as `format`, with its search paths.
    pub fn wrapping(format: &dyn PluginFormat, config: SandboxConfig) -> Self {
        Self::new(format.kind(), format.default_search_paths(), config)
    }

    /// Starts a host and loads `d` in it, keeping the concrete type (for its monitor).
    pub fn instantiate_sandboxed(
        &self,
        d: &PluginDescriptor,
        sample_rate: f64,
        max_frames: u32,
    ) -> Result<SandboxedInstance, PluginError> {
        if d.format != self.kind {
            return Err(PluginError::Load(format!(
                "a {:?} plugin given to the {:?} sandbox",
                d.format, self.kind
            )));
        }
        SandboxedInstance::new(d.clone(), self.config.clone(), sample_rate, max_frames)
    }
}

impl PluginFormat for SandboxFormat {
    fn kind(&self) -> PluginFormatKind {
        self.kind.clone()
    }

    fn default_search_paths(&self) -> Vec<PathBuf> {
        self.search_paths.clone()
    }

    fn scan(&self, path: &Path) -> Vec<PluginDescriptor> {
        self.config
            .launcher
            .scanner()
            .scan(&self.kind, path)
            .unwrap_or_default()
    }

    fn instantiate(
        &self,
        d: &PluginDescriptor,
        sample_rate: f64,
        max_frames: u32,
    ) -> Result<Box<dyn PluginInstance>, PluginError> {
        Ok(Box::new(self.instantiate_sandboxed(
            d,
            sample_rate,
            max_frames,
        )?))
    }
}

/// A registry with every format this build has, each sandboxed with `config`.
pub fn sandboxed_registry(config: &SandboxConfig) -> PluginRegistry {
    let mut registry = PluginRegistry::new();
    for format in daw_plugins::default_registry().formats() {
        registry.register(Box::new(SandboxFormat::wrapping(format, config.clone())));
    }
    registry
}
