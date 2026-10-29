//! A running CLAP plugin as a graph node.

use std::cell::RefCell;
use std::ffi::CString;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use clack_extensions::audio_ports::{AudioPortFlags, AudioPortInfoBuffer, PluginAudioPorts};
use clack_extensions::gui::{GuiApiType, GuiConfiguration, GuiSize, PluginGui, Window};
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

use crate::editor::HostWindow;
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
    /// The ports the graph connects to; other ports (sidechains, extra outputs) get silence
    /// and are discarded.
    main_in: Option<usize>,
    main_out: Option<usize>,
    note_in: bool,
    note_out: bool,
    /// The note input port only takes MIDI, so notes are converted.
    notes_as_midi: bool,
    has_params: bool,
}

impl Layout {
    fn ports(&self) -> PortLayout {
        let channels = |ports: &[usize], main: Option<usize>| {
            main.and_then(|i| ports.get(i))
                .map_or(0, |&c| u16::try_from(c).unwrap_or(u16::MAX))
        };
        PortLayout {
            audio_in: channels(&self.audio_in, self.main_in),
            audio_out: channels(&self.audio_out, self.main_out),
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
        }
    }
}

/// How this host can show the plugin's editor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EditorMode {
    None,
    /// The plugin opens its own top-level X11 window.
    Floating,
    /// The plugin embeds into an X11 window the host creates (JUCE-based plugins).
    Embedded,
}

#[derive(Default)]
struct EditorState {
    created: bool,
    /// The host window an embedded editor lives in.
    window: Option<HostWindow>,
    resizable: bool,
}

/// A CLAP plugin instance. See the crate docs for which thread may call what.
pub struct ClapInstance {
    descriptor: PluginDescriptor,
    /// Present while the plugin is active. Dropped (deactivated) before `instance`.
    processor: Option<PluginAudioProcessor<DawHost>>,
    /// In a `RefCell` only so that `param_value(&self)` can reach the main-thread handle.
    instance: RefCell<clack_host::plugin::PluginInstance<DawHost>>,
    ext: Extensions,
    editor_mode: EditorMode,
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
        let editor_mode = match ext.gui {
            Some(gui) if gui.is_api_supported(&instance.plugin_handle(), x11(true)) => {
                EditorMode::Floating
            }
            Some(gui) if gui.is_api_supported(&instance.plugin_handle(), x11(false)) => {
                EditorMode::Embedded
            }
            _ => EditorMode::None,
        };
        let mut this = Self {
            descriptor,
            processor: None,
            instance: RefCell::new(instance),
            ext,
            editor_mode,
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
                let infos: Vec<(AudioPortFlags, u32)> = (0..ports.count(&handle, is_input))
                    .map(|i| {
                        ports
                            .get(&handle, i, is_input, &mut buf)
                            .map_or((AudioPortFlags::empty(), 0), |p| (p.flags, p.channel_count))
                    })
                    .collect();
                let channels = infos.iter().map(|&(_, c)| c as usize).collect();
                let main = crate::main_port(&infos, is_input);
                if is_input {
                    layout.audio_in = channels;
                    layout.main_in = main;
                } else {
                    layout.audio_out = channels;
                    layout.main_out = main;
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
            // `stop_processing` is an audio-thread call. The engine has stopped calling `process`
            // by the time the main thread deactivates, so the main thread stands in for it.
            let stopped = {
                let _scope = AudioThreadScope::enter();
                processor.into_stopped()
            };
            self.inst().deactivate(stopped);
        }
    }

    /// What [`Node::prepare`] does, but reporting activation failure instead of leaving the
    /// plugin silent.
    pub fn try_prepare(&mut self, sample_rate: f64, max_frames: u32) -> Result<(), PluginError> {
        self.sample_rate = sample_rate;
        self.max_frames = max_frames.max(1);
        self.steady_time = 0;
        self.activate()
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
        if let Err(e) = self.try_prepare(sample_rate, max_frames) {
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

            // Audio in: the context's channels feed the main input port; others get silence.
            for (pi, port) in bufs.audio_in.iter_mut().enumerate() {
                let is_main = Some(pi) == self.layout.main_in;
                for (ch, chan) in port.chunks_exact_mut(bufs.max_frames).enumerate() {
                    match ctx.audio_in.get(ch) {
                        Some(src) if is_main && src.len() >= end => {
                            chan[..n].copy_from_slice(&src[offset..end])
                        }
                        _ => chan[..n].fill(0.0),
                    }
                }
            }

            bufs.events_in.clear();
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
            let frame_offset = u32::try_from(offset).unwrap_or(0);
            let transport = events::transport(ctx.transport, frame_offset);
            // The plugin's output events are converted straight into the context's fixed-size
            // list, so a plugin that floods events cannot make the host allocate.
            let mut sink = events::Sink {
                out: &mut *ctx.events_out,
                frame_offset,
            };
            let result = processor.process(
                &inputs,
                &mut outputs,
                &bufs.events_in.as_input(),
                &mut OutputEvents::from_buffer(&mut sink),
                Some(self.steady_time),
                Some(&transport),
            );

            // Audio out: the main output port goes to the context's channels.
            let mut ch = 0;
            if let Some(port) = self.layout.main_out.and_then(|i| bufs.audio_out.get(i)) {
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
        self.editor_mode != EditorMode::None
    }

    fn open_editor(&mut self) -> Result<(), PluginError> {
        let gui = self
            .ext
            .gui
            .ok_or_else(|| PluginError::Editor("plugin has no editor".into()))?;
        let embedded = match self.editor_mode {
            EditorMode::None => {
                return Err(PluginError::Editor("plugin has no X11 editor".into()));
            }
            EditorMode::Floating => false,
            EditorMode::Embedded => true,
        };
        let editor_err = |e: &dyn std::fmt::Display| PluginError::Editor(e.to_string());
        let instance = self.instance.get_mut();
        if !self.editor.created {
            let handle = instance.plugin_handle();
            gui.create(&handle, x11(!embedded))
                .map_err(|e| editor_err(&e))?;
            self.editor.created = true;
            if embedded {
                // X11 sizes are physical pixels, so no `set_scale`.
                let size = gui.get_size(&handle).unwrap_or(GuiSize {
                    width: 800,
                    height: 600,
                });
                let resizable = gui.can_resize(&handle);
                let window = match HostWindow::new(
                    &self.descriptor.name,
                    size.width,
                    size.height,
                    resizable,
                ) {
                    Ok(window) => window,
                    Err(e) => {
                        gui.destroy(&handle);
                        self.editor.created = false;
                        return Err(PluginError::Editor(e));
                    }
                };
                // SAFETY: the window outlives the plugin's GUI: `close_editor` destroys the GUI
                // before dropping the window.
                let parented =
                    unsafe { gui.set_parent(&handle, Window::from_x11_handle(window.id().into())) };
                if let Err(e) = parented {
                    gui.destroy(&handle);
                    self.editor.created = false;
                    return Err(editor_err(&e));
                }
                self.editor.window = Some(window);
                self.editor.resizable = resizable;
            } else if let Ok(title) = CString::new(self.descriptor.name.as_str()) {
                gui.suggest_title(&handle, &title);
            }
        }
        instance.access_shared_handler(|s| {
            HostShared::take(&s.gui_closed);
            s.pending_resize.store(0, Ordering::Relaxed);
        });
        gui.show(&instance.plugin_handle())
            .map_err(|e| editor_err(&e))?;
        if let Some(window) = &self.editor.window {
            window.show();
        }
        Ok(())
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
        // Only after `destroy`: the plugin's window is a child of this one.
        self.editor.window = None;
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
        self.pump_editor_window();
    }
}

impl ClapInstance {
    /// Applies window-manager events to an embedded editor and the plugin's resize requests to
    /// the host window.
    fn pump_editor_window(&mut self) {
        let Some(window) = self.editor.window.as_mut() else {
            return;
        };
        let events = window.pump();
        if events.closed {
            plugin::PluginInstance::close_editor(self);
            return;
        }
        let Some(gui) = self.ext.gui else {
            return;
        };
        let resizable = self.editor.resizable;
        let instance = self.instance.get_mut();
        let requested =
            instance.access_shared_handler(|s| s.pending_resize.swap(0, Ordering::AcqRel));
        let handle = instance.plugin_handle();
        let Some(window) = self.editor.window.as_mut() else {
            return;
        };
        if requested != 0 {
            #[allow(clippy::cast_possible_truncation)]
            let (w, h) = ((requested >> 32) as u32, requested as u32);
            let size = GuiSize {
                width: w,
                height: h,
            };
            if gui.set_size(&handle, size).is_ok() || !resizable {
                window.resize(w, h, resizable);
            }
        } else if let (Some((w, h)), true) = (events.resized, resizable) {
            // The user resized the window: let the plugin pick the nearest size it supports.
            let wanted = GuiSize {
                width: w,
                height: h,
            };
            let size = gui.adjust_size(&handle, wanted).unwrap_or(wanted);
            if gui.set_size(&handle, size).is_ok() && size != wanted {
                window.resize(size.width, size.height, resizable);
            }
        }
    }
}

impl Drop for ClapInstance {
    fn drop(&mut self) {
        plugin::PluginInstance::close_editor(self);
        self.deactivate();
    }
}

fn x11(is_floating: bool) -> GuiConfiguration<'static> {
    GuiConfiguration {
        api_type: GuiApiType::X11,
        is_floating,
    }
}
