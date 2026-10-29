//! A running CLAP plugin as a graph node.

use std::cell::RefCell;
use std::ffi::CString;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use clack_extensions::audio_ports::{AudioPortInfoBuffer, PluginAudioPorts};
use clack_extensions::gui::{GuiApiType, GuiConfiguration, PluginGui};
use clack_extensions::latency::PluginLatency;
use clack_extensions::note_ports::{NoteDialects, NotePortInfoBuffer, PluginNotePorts};
use clack_extensions::params::{ParamInfoBuffer, ParamInfoFlags, PluginParams};
use clack_extensions::posix_fd::{FdFlags, PluginPosixFd};
use clack_extensions::state::PluginState;
use clack_extensions::timer::PluginTimer;
use clack_host::prelude::*;
use clack_host::process::PluginAudioProcessor;
use daw_core::event::ParamId;
use daw_core::node::{Node, PortLayout, ProcessContext};
use daw_core::plugin::{self, ParamInfo, PluginDescriptor, PluginError};

use crate::events;
use crate::host::{AudioThreadScope, DawHost, HostShared};

/// Events the host can hand a plugin per block, and accept back from it. Extra events are dropped
/// instead of allocating on the audio thread.
const EVENT_CAPACITY: usize = 1024;

/// The plugin's extensions that this host uses. Each is `None` when the plugin lacks it.
#[derive(Clone, Copy)]
struct Extensions {
    audio_ports: Option<PluginAudioPorts>,
    note_ports: Option<PluginNotePorts>,
    params: Option<PluginParams>,
    state: Option<PluginState>,
    latency: Option<PluginLatency>,
    gui: Option<PluginGui>,
    timer: Option<PluginTimer>,
    posix_fd: Option<PluginPosixFd>,
}

/// The plugin's port layout, read while it is inactive.
#[derive(Clone, Debug, Default)]
struct Layout {
    /// Channel count of each input port, in port order.
    audio_in: Vec<usize>,
    /// Channel count of each output port, in port order.
    audio_out: Vec<usize>,
    note_in: bool,
    note_out: bool,
    /// The note input port only takes MIDI, so notes are converted.
    notes_as_midi: bool,
    has_params: bool,
}

impl Layout {
    fn ports(&self) -> PortLayout {
        let sum = |v: &[usize]| u16::try_from(v.iter().sum::<usize>()).unwrap_or(u16::MAX);
        PortLayout {
            audio_in: sum(&self.audio_in),
            audio_out: sum(&self.audio_out),
            event_in: self.note_in || self.has_params,
            event_out: self.note_out || self.has_params,
        }
    }
}

/// Everything `process` touches, allocated in `prepare`.
struct Buffers {
    max_frames: usize,
    /// One allocation per input port, `channels * max_frames` long.
    audio_in: Vec<Vec<f32>>,
    audio_out: Vec<Vec<f32>>,
    in_ports: AudioPorts,
    out_ports: AudioPorts,
    events_in: EventBuffer,
    events_out: EventBuffer,
}

impl Buffers {
    fn new(layout: &Layout, max_frames: usize) -> Self {
        let alloc = |ports: &[usize]| -> Vec<Vec<f32>> {
            ports.iter().map(|c| vec![0.0; c * max_frames]).collect()
        };
        Self {
            max_frames,
            audio_in: alloc(&layout.audio_in),
            audio_out: alloc(&layout.audio_out),
            in_ports: AudioPorts::with_capacity(
                layout.audio_in.iter().sum(),
                layout.audio_in.len(),
            ),
            out_ports: AudioPorts::with_capacity(
                layout.audio_out.iter().sum(),
                layout.audio_out.len(),
            ),
            events_in: EventBuffer::with_capacity(EVENT_CAPACITY),
            events_out: EventBuffer::with_capacity(EVENT_CAPACITY),
        }
    }
}

#[derive(Default)]
struct EditorState {
    created: bool,
}

/// A CLAP plugin instance. See the crate docs for which thread may call what.
pub struct ClapInstance {
    descriptor: PluginDescriptor,
    /// Present while the plugin is active. Dropped (deactivated) before `instance`.
    processor: Option<PluginAudioProcessor<DawHost>>,
    /// In a `RefCell` only so that `param_value(&self)` can reach the main-thread handle.
    instance: RefCell<clack_host::plugin::PluginInstance<DawHost>>,
    ext: Extensions,
    editor_supported: bool,
    layout: Layout,
    sample_rate: f64,
    max_frames: u32,
    latency: u32,
    buffers: Option<Buffers>,
    steady_time: u64,
    editor: EditorState,
    params: Vec<ParamInfo>,
}

// SAFETY: the CLAP threading rules are upheld by the contract documented on the crate: every
// method other than `process` and `reset` runs on the thread that created the instance, and
// `&mut self` keeps `process` from overlapping with them. The plugin's audio-thread functions
// are only reached through `process` and `reset`.
unsafe impl Send for ClapInstance {}

impl ClapInstance {
    /// Creates an inactive instance of the plugin `descriptor.id` from `entry`. It is activated
    /// by [`Node::prepare`].
    pub fn new(entry: &PluginEntry, descriptor: PluginDescriptor) -> Result<Self, PluginError> {
        let id = CString::new(descriptor.id.as_str())
            .map_err(|_| PluginError::Load(format!("bad plugin id {:?}", descriptor.id)))?;
        let mut instance = crate::host::new_instance(entry, &id)
            .map_err(|e| PluginError::Load(format!("{}: {e}", descriptor.id)))?;
        let shared = instance.plugin_shared_handle();
        let ext = Extensions {
            audio_ports: shared.get_extension(),
            note_ports: shared.get_extension(),
            params: shared.get_extension(),
            state: shared.get_extension(),
            latency: shared.get_extension(),
            gui: shared.get_extension(),
            timer: shared.get_extension(),
            posix_fd: shared.get_extension(),
        };
        let editor_supported = ext
            .gui
            .is_some_and(|gui| gui.is_api_supported(&instance.plugin_handle(), floating_x11()));
        let mut this = Self {
            descriptor,
            processor: None,
            instance: RefCell::new(instance),
            ext,
            editor_supported,
            layout: Layout::default(),
            sample_rate: 0.0,
            max_frames: 0,
            latency: 0,
            buffers: None,
            steady_time: 0,
            editor: EditorState::default(),
            params: Vec::new(),
        };
        this.layout = this.read_layout();
        this.params = this.read_params();
        Ok(this)
    }

    fn read_layout(&mut self) -> Layout {
        let ext = self.ext;
        let handle = self.inst().plugin_handle();
        let mut layout = Layout::default();
        if let Some(ports) = ext.audio_ports {
            let mut buf = AudioPortInfoBuffer::new();
            for is_input in [true, false] {
                let channels: Vec<usize> = (0..ports.count(&handle, is_input))
                    .map(|i| {
                        ports
                            .get(&handle, i, is_input, &mut buf)
                            .map_or(0, |p| p.channel_count as usize)
                    })
                    .collect();
                if is_input {
                    layout.audio_in = channels;
                } else {
                    layout.audio_out = channels;
                }
            }
        }
        if let Some(notes) = ext.note_ports {
            let mut buf = NotePortInfoBuffer::new();
            if notes.count(&handle, true) > 0 {
                layout.note_in = true;
                if let Some(info) = notes.get(&handle, 0, true, &mut buf) {
                    layout.notes_as_midi = !info.supported_dialects.contains(NoteDialects::CLAP)
                        && info.supported_dialects.contains(NoteDialects::MIDI);
                }
            }
            layout.note_out = notes.count(&handle, false) > 0;
        }
        layout.has_params = ext.params.is_some_and(|p| p.count(&handle) > 0);
        layout
    }

    fn read_params(&mut self) -> Vec<ParamInfo> {
        let Some(params) = self.ext.params else {
            return Vec::new();
        };
        let handle = self.inst().plugin_handle();
        let mut buf = ParamInfoBuffer::new();
        (0..params.count(&handle))
            .filter_map(|i| {
                let info = params.get_info(&handle, i, &mut buf)?;
                if info.flags.contains(ParamInfoFlags::IS_HIDDEN) {
                    return None;
                }
                Some(ParamInfo {
                    id: info.id.get(),
                    name: String::from_utf8_lossy(info.name).into_owned(),
                    min: info.min_value,
                    max: info.max_value,
                    default: info.default_value,
                    automatable: info.flags.contains(ParamInfoFlags::IS_AUTOMATABLE)
                        && !info.flags.contains(ParamInfoFlags::IS_READONLY),
                })
            })
            .collect()
    }

    fn take_flag(&mut self, flag: impl Fn(&HostShared) -> &AtomicBool) -> bool {
        self.inst()
            .access_shared_handler(|s| HostShared::take(flag(s)))
    }

    fn inst(&mut self) -> &mut clack_host::plugin::PluginInstance<DawHost> {
        self.instance.get_mut()
    }

    fn deactivate(&mut self) {
        if let Some(processor) = self.processor.take() {
            let stopped = processor.into_stopped();
            self.inst().deactivate(stopped);
        }
    }

    fn activate(&mut self) -> Result<(), PluginError> {
        self.deactivate();
        // Ports may only change while inactive, so this is the moment to re-read them.
        self.layout = self.read_layout();
        let config = PluginAudioConfiguration {
            sample_rate: self.sample_rate,
            min_frames_count: 1,
            max_frames_count: self.max_frames,
        };
        let stopped = self
            .inst()
            .activate(|_, _| (), config)
            .map_err(|e| PluginError::Load(format!("activate {}: {e}", self.descriptor.id)))?;
        self.processor = Some(PluginAudioProcessor::Stopped(stopped));
        self.buffers = Some(Buffers::new(&self.layout, self.max_frames as usize));
        self.latency = self.read_latency();
        Ok(())
    }

    fn read_latency(&mut self) -> u32 {
        let latency = self.ext.latency;
        latency.map_or(0, |l| l.get(&self.inst().plugin_handle()))
    }

    /// Runs due timers and ready file descriptors. Returns nothing; plugin callbacks may register
    /// or remove timers and descriptors while this runs, so they are collected first.
    fn service_timers_and_fds(&mut self) {
        if let Some(timer) = self.ext.timer {
            let now = Instant::now();
            let due: Vec<_> = self.inst().access_handler(|h| {
                let mut timers = h.timers.borrow_mut();
                timers
                    .iter_mut()
                    .filter(|t| t.next <= now)
                    .map(|t| {
                        t.next = now + t.period;
                        t.id
                    })
                    .collect()
            });
            for id in due {
                timer.on_timer(&self.inst().plugin_handle(), id);
            }
        }
        if let Some(posix_fd) = self.ext.posix_fd {
            let fds = self.inst().access_handler(|h| h.fds.borrow().clone());
            if fds.is_empty() {
                return;
            }
            let mut pollfds: Vec<libc::pollfd> = fds
                .iter()
                .map(|&(fd, flags)| {
                    let mut events = 0;
                    if flags.contains(FdFlags::READ) {
                        events |= libc::POLLIN;
                    }
                    if flags.contains(FdFlags::WRITE) {
                        events |= libc::POLLOUT;
                    }
                    libc::pollfd {
                        fd,
                        events,
                        revents: 0,
                    }
                })
                .collect();
            // SAFETY: `pollfds` is a valid, initialised slice for the duration of the call.
            let ready =
                unsafe { libc::poll(pollfds.as_mut_ptr(), pollfds.len() as libc::nfds_t, 0) };
            if ready <= 0 {
                return;
            }
            for p in pollfds.iter().filter(|p| p.revents != 0) {
                let mut flags = FdFlags::empty();
                if p.revents & libc::POLLIN != 0 {
                    flags |= FdFlags::READ;
                }
                if p.revents & libc::POLLOUT != 0 {
                    flags |= FdFlags::WRITE;
                }
                if p.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
                    flags |= FdFlags::ERROR;
                }
                posix_fd.on_fd(&self.inst().plugin_handle(), p.fd, flags);
            }
        }
    }

    fn silence(ctx: &mut ProcessContext) {
        let frames = ctx.frames as usize;
        for out in ctx.audio_out.iter_mut() {
            let n = frames.min(out.len());
            out[..n].fill(0.0);
        }
    }
}

impl Node for ClapInstance {
    fn ports(&self) -> PortLayout {
        self.layout.ports()
    }

    fn prepare(&mut self, sample_rate: f64, max_frames: u32) {
        self.sample_rate = sample_rate;
        self.max_frames = max_frames.max(1);
        self.steady_time = 0;
        if let Err(e) = self.activate() {
            // `Node::prepare` cannot fail; an inactive plugin outputs silence.
            eprintln!("{e}");
        }
    }

    fn process(&mut self, ctx: &mut ProcessContext) {
        let _scope = AudioThreadScope::enter();
        let (Some(processor), Some(bufs)) = (self.processor.as_mut(), self.buffers.as_mut()) else {
            Self::silence(ctx);
            return;
        };
        let Ok(processor) = processor.ensure_processing_started() else {
            Self::silence(ctx);
            return;
        };

        let total = ctx.frames as usize;
        let mut pending = ctx.events_in.iter().peekable();
        let mut offset = 0usize;
        while offset < total {
            let n = (total - offset).min(bufs.max_frames);
            let end = offset + n;
            let last = end == total;

            // Audio in: the context's channels are the plugin's ports' channels, flattened.
            let mut ch = 0;
            for port in bufs.audio_in.iter_mut() {
                for chan in port.chunks_exact_mut(bufs.max_frames) {
                    match ctx.audio_in.get(ch) {
                        Some(src) if src.len() >= end => {
                            chan[..n].copy_from_slice(&src[offset..end])
                        }
                        _ => chan[..n].fill(0.0),
                    }
                    ch += 1;
                }
            }

            bufs.events_in.clear();
            bufs.events_out.clear();
            while let Some(e) = pending.peek() {
                let frame = e.frame as usize;
                if frame >= end && !last {
                    break;
                }
                if (bufs.events_in.len() as usize) < EVENT_CAPACITY {
                    let at = u32::try_from(frame.saturating_sub(offset).min(n - 1)).unwrap_or(0);
                    events::push_clap(&mut bufs.events_in, e, at, self.layout.notes_as_midi);
                }
                pending.next();
            }

            let max = bufs.max_frames;
            let inputs = bufs
                .in_ports
                .with_input_buffers(bufs.audio_in.iter_mut().map(|port| AudioPortBuffer {
                    latency: 0,
                    channels: AudioPortBufferType::f32_input_only(port.chunks_exact_mut(max).map(
                        |c| InputChannel {
                            buffer: &mut c[..n],
                            is_constant: false,
                        },
                    )),
                }));
            let mut outputs = bufs
                .out_ports
                .with_output_buffers(bufs.audio_out.iter_mut().map(|port| AudioPortBuffer {
                    latency: 0,
                    channels: AudioPortBufferType::f32_output_only(
                        port.chunks_exact_mut(max).map(|c| &mut c[..n]),
                    ),
                }));
            let transport = events::transport(ctx.transport, u32::try_from(offset).unwrap_or(0));
            let result = processor.process(
                &inputs,
                &mut outputs,
                &bufs.events_in.as_input(),
                &mut bufs.events_out.as_output(),
                Some(self.steady_time),
                Some(&transport),
            );

            let mut ch = 0;
            for port in &bufs.audio_out {
                for chan in port.chunks_exact(bufs.max_frames) {
                    if let Some(dst) = ctx.audio_out.get_mut(ch)
                        && dst.len() >= end
                    {
                        if result.is_ok() {
                            dst[offset..end].copy_from_slice(&chan[..n]);
                        } else {
                            dst[offset..end].fill(0.0);
                        }
                    }
                    ch += 1;
                }
            }
            for dst in ctx.audio_out.iter_mut().skip(ch) {
                if dst.len() >= end {
                    dst[offset..end].fill(0.0);
                }
            }

            let frame_offset = u32::try_from(offset).unwrap_or(0);
            for e in bufs.events_out.iter() {
                if let Some(e) = events::from_clap(e, frame_offset) {
                    let _ = ctx.events_out.push(e);
                }
            }

            self.steady_time += n as u64;
            offset = end;
        }
    }

    fn latency(&self) -> u32 {
        self.latency
    }

    fn reset(&mut self) {
        if let Some(processor) = self.processor.as_mut() {
            let _scope = AudioThreadScope::enter();
            processor.reset();
        }
    }
}

impl plugin::PluginInstance for ClapInstance {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    fn params(&self) -> Vec<ParamInfo> {
        self.params.clone()
    }

    fn param_value(&self, id: ParamId) -> f64 {
        let value = self.ext.params.and_then(|params| {
            let mut instance = self.instance.borrow_mut();
            params.get_value(&instance.plugin_handle(), ClapId::new(id))
        });
        value.unwrap_or_else(|| {
            self.params
                .iter()
                .find(|p| p.id == id)
                .map_or(0.0, |p| p.default)
        })
    }

    fn save_state(&mut self) -> Result<Vec<u8>, PluginError> {
        let state = self
            .ext
            .state
            .ok_or_else(|| PluginError::State("plugin has no state extension".into()))?;
        let mut data = Vec::new();
        state
            .save(&self.inst().plugin_handle(), &mut data)
            .map_err(|e| PluginError::State(e.to_string()))?;
        self.take_flag(|s| &s.state_dirty);
        Ok(data)
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), PluginError> {
        let state = self
            .ext
            .state
            .ok_or_else(|| PluginError::State("plugin has no state extension".into()))?;
        let mut reader = data;
        state
            .load(&self.inst().plugin_handle(), &mut reader)
            .map_err(|e| PluginError::State(e.to_string()))?;
        // Loading state can change parameters, latency and even ports.
        self.params = self.read_params();
        if self.processor.is_some() {
            self.latency = self.read_latency();
        }
        Ok(())
    }

    fn has_editor(&self) -> bool {
        self.editor_supported
    }

    fn open_editor(&mut self) -> Result<(), PluginError> {
        let gui = self
            .ext
            .gui
            .ok_or_else(|| PluginError::Editor("plugin has no editor".into()))?;
        if !self.editor_supported {
            return Err(PluginError::Editor(
                "plugin has no floating X11 editor".into(),
            ));
        }
        let instance = self.instance.get_mut();
        if !self.editor.created {
            let handle = instance.plugin_handle();
            gui.create(&handle, floating_x11())
                .map_err(|e| PluginError::Editor(e.to_string()))?;
            if let Ok(title) = CString::new(self.descriptor.name.as_str()) {
                gui.suggest_title(&handle, &title);
            }
            self.editor.created = true;
        }
        instance.access_shared_handler(|s| HostShared::take(&s.gui_closed));
        gui.show(&instance.plugin_handle())
            .map_err(|e| PluginError::Editor(e.to_string()))
    }

    fn close_editor(&mut self) {
        if !self.editor.created {
            return;
        }
        if let Some(gui) = self.ext.gui {
            let handle = self.inst().plugin_handle();
            let _ = gui.hide(&handle);
            gui.destroy(&handle);
        }
        self.editor.created = false;
    }

    fn on_main_thread(&mut self) {
        let callback = self.take_flag(|s| &s.callback_requested);
        let restart = self.take_flag(|s| &s.restart_requested);
        let flush = self.take_flag(|s| &s.flush_requested);
        let gui_closed = self.take_flag(|s| &s.gui_closed);

        if callback {
            self.inst().call_on_main_thread_callback();
        }
        if gui_closed && self.editor.created {
            // The user closed the floating window; release the editor's resources.
            plugin::PluginInstance::close_editor(self);
        }
        if flush
            && self.processor.is_none()
            && let (Some(params), Some(mut handle)) =
                (self.ext.params, self.inst().inactive_plugin_handle())
        {
            let mut out = EventBuffer::new();
            params.flush(&mut handle, &InputEvents::empty(), &mut out.as_output());
        }
        let (latency_changed, rescan) = self.inst().access_handler(|h| {
            (
                h.latency_changed.replace(false),
                h.params_rescan.replace(false),
            )
        });
        if rescan {
            self.params = self.read_params();
        }
        if restart && self.processor.is_some() {
            if let Err(e) = self.activate() {
                eprintln!("{e}");
            }
        } else if latency_changed && self.processor.is_some() {
            self.latency = self.read_latency();
        }
        self.service_timers_and_fds();
    }
}

impl Drop for ClapInstance {
    fn drop(&mut self) {
        plugin::PluginInstance::close_editor(self);
        self.deactivate();
    }
}

fn floating_x11() -> GuiConfiguration<'static> {
    GuiConfiguration {
        api_type: GuiApiType::X11,
        is_floating: true,
    }
}
