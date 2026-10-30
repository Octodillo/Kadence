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
//! - A watchdog thread notices when the host exits or an audio cycle stalls. The instance then
//!   outputs silence, its [`SandboxMonitor`] reports [`SandboxStatus::Crashed`], and the
//!   [`SandboxConfig::on_crash`] hook is called, so the UI can offer a reload.
//! - [`SandboxedInstance::reload`] (or [`SandboxMonitor::request_reload`], performed on the next
//!   `on_main_thread`) starts a new host and restores the last saved or loaded state plus the
//!   parameter changes made since.
//!
//! [`SandboxFormat`] wraps a format so a [`daw_plugins::PluginRegistry`] hands out sandboxed
//! instances; [`sandboxed_registry`] does that for every format in this build.

mod child;
mod client;
mod format;
mod protocol;
mod shm;

pub use child::{HOST_ARG, run_host_child};
pub use client::{
    CrashHook, CrashReason, HostLauncher, SandboxConfig, SandboxMonitor, SandboxStatus,
    SandboxedInstance,
};
pub use format::{SandboxFormat, sandboxed_registry};
pub use shm::{EVENT_CAPACITY, MAX_CHANNELS};
