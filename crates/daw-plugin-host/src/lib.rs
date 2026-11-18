//! Runs plugins out of process, so a plugin that crashes or hangs cannot take the session down.
//!
//! Each [`SandboxedInstance`] starts its own host process (the `daw-plugin-host` binary, or the
//! application itself if it forwards `--plugin-host` to [`run_host_child`]), which loads the
//! plugin with the ordinary in-process format adapters.
//!
//! - Main-thread calls (state, params, editor) go over a Unix socket.
//! - Audio, events and transport go through a shared-memory block. `process` hands each cycle
//!   to the host with a futex and waits for it with a deadline, without allocating or locking;
//!   the host processes on its own audio thread.
//! - A plugin whose main thread waits on the user in its own editor (a modal file dialog, say)
//!   is busy, not hung: calls give up on it with [`PluginError::Busy`] and leave it running.
//! - One supervisor thread for all hosts notices when a host exits or an audio cycle stalls,
//!   and a host is killed together with any helper process it started. The instance then
//!   outputs silence, its [`SandboxMonitor`] reports [`SandboxStatus::Crashed`], and the
//!   [`SandboxConfig::on_crash`] hook is called, so the UI can offer a reload.
//! - [`SandboxedInstance::reload`] (or [`SandboxMonitor::request_reload`], performed on the next
//!   `on_main_thread`) starts a new host and restores the last saved or loaded state plus the
//!   parameter changes made since.
//!
//! [`SandboxFormat`] is a format whose instances are sandboxed, for a
//! [`daw_plugins::PluginRegistry`] or to load one plugin directly.
//!
//! [`PluginError::Busy`]: daw_core::plugin::PluginError::Busy

mod child;
mod client;
mod control;
mod format;
mod protocol;
mod shm;
mod supervisor;

/// Locks a mutex whose data stays usable after a panic elsewhere.
fn locked<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub use child::{HOST_ARG, run_host_child};
pub use client::{
    CrashHook, CrashReason, HostLauncher, SandboxConfig, SandboxMonitor, SandboxStatus,
    SandboxedInstance,
};
pub use format::SandboxFormat;
pub use shm::{EVENT_CAPACITY, MAX_CHANNELS};
