//! Plugin UIs as floating windows.
//!
//! `ui:X11UI` UIs are embedded in a top-level X11 window we create (through
//! XWayland on a Wayland session). kxstudio external UIs make their own
//! window and only need to be shown, run and hidden. Everything here runs on
//! the main thread.

use std::ffi::{CString, c_char, c_int, c_void};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use daw_core::plugin::PluginError;
use daw_plugin_window::x11rb::connection::Connection;
use daw_plugin_window::x11rb::protocol::Event as XEvent;
use daw_plugin_window::x11rb::protocol::xproto::{ConfigureWindowAux, ConnectionExt as _};
use daw_plugin_window::{HostWindow, WindowEvent};
use lv2_sys::{
    LV2_Extension_Data_Feature, LV2_Feature, LV2UI_Descriptor, LV2UI_Handle, LV2UI_Idle_Interface,
    LV2UI_Resize,
};
use rtrb::{Consumer, Producer};

use crate::atom::Urids;
use crate::{ring, uri_to_path};

const X11_UI: &str = "http://lv2plug.in/ns/extensions/ui#X11UI";
const KX_WIDGET: &str = "http://kxstudio.sf.net/ns/lv2ext/external-ui#Widget";
const KX_HOST: &[u8] = b"http://kxstudio.sf.net/ns/lv2ext/external-ui#Host\0";
const OLD_EXTERNAL: &str = "http://lv2plug.in/ns/extensions/ui#external";
const OLD_EXTERNAL_HOST: &[u8] = b"http://lv2plug.in/ns/extensions/ui#external\0";
/// UI features saying the user may not resize the UI.
const NO_USER_RESIZE: &str = "http://lv2plug.in/ns/extensions/ui#noUserResize";
const FIXED_SIZE: &str = "http://lv2plug.in/ns/extensions/ui#fixedSize";
const URID_MAP: &str = "http://lv2plug.in/ns/ext/urid#map";
const URID_UNMAP: &str = "http://lv2plug.in/ns/ext/urid#unmap";
const OPTIONS: &str = "http://lv2plug.in/ns/ext/options#options";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UiKind {
    X11,
    External,
}

/// The UI we would open for a plugin.
#[derive(Clone, Debug)]
pub(crate) struct UiInfo {
    uri: String,
    kind: UiKind,
    binary: PathBuf,
    bundle: PathBuf,
    /// The UI lists ui:noUserResize or ui:fixedSize: its window gets a fixed
    /// size and the UI's own widget is never resized by us.
    fixed_size: bool,
}

impl UiInfo {
    /// Picks the plugin's X11 UI, or failing that an external one.
    pub(crate) fn find(world: &livi::World, plugin: &livi::Plugin) -> Option<UiInfo> {
        let lilv = world.raw();
        let x11 = lilv.new_uri(X11_UI);
        let kx = lilv.new_uri(KX_WIDGET);
        let old = lilv.new_uri(OLD_EXTERNAL);
        let no_user_resize = lilv.new_uri(NO_USER_RESIZE);
        let fixed_size = lilv.new_uri(FIXED_SIZE);
        let mut found: Option<UiInfo> = None;
        for ui in plugin.raw().uis()?.iter() {
            let kind = if ui.is_a(&x11) {
                UiKind::X11
            } else if ui.is_a(&kx) || ui.is_a(&old) {
                UiKind::External
            } else {
                continue;
            };
            let path = |n: Option<livi::lilv::node::Node>| n?.as_uri().and_then(uri_to_path);
            let (Some(binary), Some(bundle)) = (path(ui.binary_uri()), path(ui.bundle_uri()))
            else {
                continue;
            };
            let Some(uri) = ui.uri().as_uri().map(str::to_owned) else {
                continue;
            };
            // UI data often lives in a file of its own: make sure it is
            // loaded. The features may be optional or required, so any
            // predicate counts.
            let node = ui.uri();
            let _ = lilv.load_resource(&node);
            let fixed_size = lilv.ask(Some(&node), None, Some(&no_user_resize))
                || lilv.ask(Some(&node), None, Some(&fixed_size));
            let info = UiInfo {
                uri,
                kind,
                binary,
                bundle,
                fixed_size,
            };
            if kind == UiKind::X11 {
                return Some(info);
            }
            found.get_or_insert(info);
        }
        found
    }
}

/// The UI side of the two rings between UI and audio thread.
pub(crate) struct UiEnds {
    pub to_dsp: Producer<u8>,
    pub from_dsp: Consumer<u8>,
}

/// What the editor needs from its instance.
pub(crate) struct HostRefs<'a> {
    pub features: &'a Arc<livi::Features>,
    pub urids: &'a Urids,
    pub plugin_uri: &'a str,
    pub title: &'a str,
    pub handle: *mut c_void,
    pub extension_data: Option<extern "C" fn(*const c_char) -> *const c_void>,
}

#[repr(C)]
struct ExternalWidget {
    run: Option<unsafe extern "C" fn(*mut ExternalWidget)>,
    show: Option<unsafe extern "C" fn(*mut ExternalWidget)>,
    hide: Option<unsafe extern "C" fn(*mut ExternalWidget)>,
}

#[repr(C)]
struct ExternalHost {
    ui_closed: Option<unsafe extern "C" fn(controller: *mut c_void)>,
    plugin_human_id: *const c_char,
}

struct X11 {
    host: HostWindow,
    /// The UI's widget, once it has made one.
    child: u32,
    /// The UI has asked for a size, or the window took its widget's.
    sized: bool,
}

impl X11 {
    /// A `fixed` UI may not be resized by the user (see `UiInfo::fixed_size`).
    fn open(title: &str, fixed: bool) -> Result<Self, PluginError> {
        // The size is a placeholder until the UI asks for one or makes its widget.
        Ok(Self {
            host: HostWindow::new(title, 400, 300, fixed)?,
            child: 0,
            sized: false,
        })
    }

    fn resize(&mut self, w: u32, h: u32) {
        if w == 0 || h == 0 || (self.sized && (w, h) == self.host.size()) {
            return;
        }
        self.sized = true;
        self.host.resize(w, h);
    }
}

/// Loads a UI library once and never unloads it: unloading toolkit-based UI
/// libraries is a well-known source of crashes in LV2 hosts (their thread-local
/// and atexit destructors outlive the code).
fn ui_library(path: &Path) -> Result<&'static libloading::Library, String> {
    static LIBRARIES: Mutex<Vec<(PathBuf, &'static libloading::Library)>> = Mutex::new(Vec::new());
    let mut libs = LIBRARIES.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, l)) = libs.iter().find(|(p, _)| p == path) {
        return Ok(l);
    }
    // SAFETY: loading a UI library runs its initialisers; that is inherent to
    // hosting native plugins.
    let l = unsafe { libloading::Library::new(path) }
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let l: &'static libloading::Library = Box::leak(Box::new(l));
    libs.push((path.to_path_buf(), l));
    Ok(l)
}

/// What the UI's callbacks reach through their handle. Lives in a Box whose
/// address the UI keeps until cleanup.
struct Controller {
    to_dsp: Producer<u8>,
    event_transfer: u32,
    closed: bool,
    x11: Option<X11>,
    /// The control values last given to the UI, so a UI that writes back what it was given
    /// does not count as an edit.
    last_sent: Vec<(u32, f32)>,
    /// The UI wrote something new since `take_state_changed`.
    changed: bool,
}

unsafe extern "C" fn write_function(
    controller: *mut c_void,
    port: u32,
    size: u32,
    protocol: u32,
    buffer: *const c_void,
) {
    // SAFETY: `controller` is the Box we passed at instantiation; the UI only
    // calls back on the main thread while the editor is alive.
    let c = unsafe { &mut *controller.cast::<Controller>() };
    if buffer.is_null() {
        return;
    }
    // SAFETY: the UI promises `size` readable bytes at `buffer`.
    let bytes = unsafe { std::slice::from_raw_parts(buffer.cast::<u8>(), size as usize) };
    if protocol == 0 && size == 4 {
        let value = f32::from_ne_bytes(bytes.try_into().unwrap_or_default());
        let echo = c
            .last_sent
            .iter()
            .any(|&(p, v)| p == port && v.to_bits() == value.to_bits());
        if !echo {
            c.changed = true;
        }
        ring::write(&mut c.to_dsp, port, ring::CONTROL, bytes);
    } else if protocol == c.event_transfer {
        c.changed = true;
        ring::write(&mut c.to_dsp, port, ring::ATOM, bytes);
    }
}

unsafe extern "C" fn host_resize(handle: *mut c_void, width: c_int, height: c_int) -> c_int {
    // SAFETY: as for `write_function`.
    let c = unsafe { &mut *handle.cast::<Controller>() };
    match &mut c.x11 {
        Some(x) => {
            x.resize(width.max(0) as u32, height.max(0) as u32);
            0
        }
        None => 1,
    }
}

unsafe extern "C" fn external_closed(controller: *mut c_void) {
    // SAFETY: as for `write_function`.
    unsafe { (*controller.cast::<Controller>()).closed = true };
}

/// An open plugin UI.
pub(crate) struct Editor {
    descriptor: *const LV2UI_Descriptor,
    handle: LV2UI_Handle,
    controller: *mut Controller,
    from_dsp: Consumer<u8>,
    idle: Option<*const LV2UI_Idle_Interface>,
    ui_resize: Option<*const LV2UI_Resize>,
    external: Option<*mut ExternalWidget>,
    atom_buf: Vec<u8>,
    event_transfer: u32,
    // Everything the UI may still point at, kept until cleanup.
    _features: Box<FeatureStore>,
    _host_features: Arc<livi::Features>,
}

// SAFETY: an editor is only ever used from the main thread; it moves between
// threads only as part of its instance, never while in use.
unsafe impl Send for Editor {}

struct FeatureStore {
    _strings: Vec<CString>,
    features: Vec<LV2_Feature>,
    _data_access: Box<LV2_Extension_Data_Feature>,
    _resize: Box<LV2UI_Resize>,
    _external: Box<ExternalHost>,
}

impl Editor {
    pub(crate) fn open(
        info: &UiInfo,
        host: HostRefs<'_>,
        ends: UiEnds,
    ) -> Result<Editor, (PluginError, UiEnds)> {
        let UiEnds { to_dsp, from_dsp } = ends;
        let from_dsp_capacity = from_dsp.buffer().capacity();
        let controller = Box::into_raw(Box::new(Controller {
            to_dsp,
            event_transfer: host.urids.event_transfer,
            closed: false,
            x11: None,
            last_sent: Vec::new(),
            changed: false,
        }));
        match Self::try_open(info, &host, controller) {
            Ok((descriptor, handle, features, external)) => {
                let mut editor = Editor {
                    descriptor,
                    handle,
                    controller,
                    from_dsp,
                    idle: None,
                    ui_resize: None,
                    external,
                    atom_buf: vec![0; from_dsp_capacity],
                    event_transfer: host.urids.event_transfer,
                    _features: features,
                    _host_features: host.features.clone(),
                };
                editor.after_instantiate();
                Ok(editor)
            }
            Err(e) => {
                // SAFETY: the UI was not created (or was cleaned up), so
                // nothing refers to the controller any more.
                let c = unsafe { Box::from_raw(controller) };
                Err((
                    e,
                    UiEnds {
                        to_dsp: c.to_dsp,
                        from_dsp,
                    },
                ))
            }
        }
    }

    #[allow(clippy::type_complexity)]
    fn try_open(
        info: &UiInfo,
        host: &HostRefs<'_>,
        controller: *mut Controller,
    ) -> Result<
        (
            *const LV2UI_Descriptor,
            LV2UI_Handle,
            Box<FeatureStore>,
            Option<*mut ExternalWidget>,
        ),
        PluginError,
    > {
        let library = ui_library(&info.binary).map_err(PluginError::Editor)?;
        type DescriptorFn = unsafe extern "C" fn(u32) -> *const LV2UI_Descriptor;
        // SAFETY: `lv2ui_descriptor` has this signature in every LV2 UI binary.
        let lookup: DescriptorFn = unsafe {
            *library
                .get::<DescriptorFn>(b"lv2ui_descriptor\0")
                .map_err(|e| PluginError::Editor(e.to_string()))?
        };
        let descriptor = (0..)
            .map(|i| {
                // SAFETY: called with increasing indices until it returns null.
                unsafe { lookup(i) }
            })
            .take_while(|d| !d.is_null())
            .find(|&d| {
                // SAFETY: non-null descriptors have a valid URI string.
                let uri = unsafe { std::ffi::CStr::from_ptr((*d).URI) };
                uri.to_bytes() == info.uri.as_bytes()
            })
            .ok_or_else(|| {
                PluginError::Editor(format!("{} is not in {}", info.uri, info.binary.display()))
            })?;

        // The UI's callbacks reach the controller through its pointer at any
        // time we call into the UI, so it is only ever touched through short
        // dereferences here, never a reference held across a UI call.
        if info.kind == UiKind::X11 {
            let x11 = X11::open(host.title, info.fixed_size)?;
            // SAFETY: `controller` is valid until we free it; no UI exists yet.
            unsafe { (*controller).x11 = Some(x11) };
        }
        // SAFETY: as above.
        let parent = unsafe { (*controller).x11.as_ref().map(|x| x.host.id()) };

        let title = CString::new(host.title).unwrap_or_default();
        let mut external_host = Box::new(ExternalHost {
            ui_closed: Some(external_closed),
            plugin_human_id: title.as_ptr(),
        });
        let mut data_access = Box::new(LV2_Extension_Data_Feature {
            // SAFETY: both are `extern "C" fn(*const c_char) -> *const c_void`.
            data_access: host.extension_data.map(|f| unsafe {
                std::mem::transmute::<
                    extern "C" fn(*const c_char) -> *const c_void,
                    unsafe extern "C" fn(*const c_char) -> *const c_void,
                >(f)
            }),
        });
        let mut resize = Box::new(LV2UI_Resize {
            handle: controller.cast(),
            ui_resize: Some(host_resize),
        });
        let uri = |s: &[u8]| s.as_ptr().cast::<c_char>();
        let mut features: Vec<LV2_Feature> = Vec::new();
        let dummy_worker = lv2_raw::LV2Feature {
            uri: uri(b"urn:daw:none\0"),
            data: std::ptr::null_mut(),
        };
        for f in host.features.iter_features(&dummy_worker) {
            // SAFETY: livi's feature URIs are NUL-terminated static strings.
            let u = unsafe { std::ffi::CStr::from_ptr(f.uri) }.to_bytes();
            if [URID_MAP, URID_UNMAP, OPTIONS]
                .iter()
                .any(|k| k.as_bytes() == u)
            {
                features.push(LV2_Feature {
                    URI: f.uri,
                    data: f.data,
                });
            }
        }
        features.push(LV2_Feature {
            URI: uri(lv2_sys::LV2_INSTANCE_ACCESS_URI),
            data: host.handle,
        });
        features.push(LV2_Feature {
            URI: uri(lv2_sys::LV2_DATA_ACCESS_URI),
            data: (&mut *data_access as *mut LV2_Extension_Data_Feature).cast(),
        });
        features.push(LV2_Feature {
            URI: uri(lv2_sys::LV2_UI__idleInterface),
            data: std::ptr::null_mut(),
        });
        if let Some(window) = parent {
            features.push(LV2_Feature {
                URI: uri(lv2_sys::LV2_UI__parent),
                data: window as usize as *mut c_void,
            });
            features.push(LV2_Feature {
                URI: uri(lv2_sys::LV2_UI__resize),
                data: (&mut *resize as *mut LV2UI_Resize).cast(),
            });
        } else {
            for u in [KX_HOST, OLD_EXTERNAL_HOST] {
                features.push(LV2_Feature {
                    URI: uri(u),
                    data: (&mut *external_host as *mut ExternalHost).cast(),
                });
            }
        }
        let mut store = Box::new(FeatureStore {
            _strings: vec![title],
            features,
            _data_access: data_access,
            _resize: resize,
            _external: external_host,
        });
        let mut ptrs: Vec<*const LV2_Feature> = store
            .features
            .iter()
            .map(|f| f as *const LV2_Feature)
            .collect();
        ptrs.push(std::ptr::null());

        let plugin_uri =
            CString::new(host.plugin_uri).map_err(|e| PluginError::Editor(e.to_string()))?;
        let mut bundle = info.bundle.to_string_lossy().into_owned();
        if !bundle.ends_with('/') {
            bundle.push('/');
        }
        let bundle = CString::new(bundle).map_err(|e| PluginError::Editor(e.to_string()))?;
        let mut widget: *mut c_void = std::ptr::null_mut();
        // SAFETY: `descriptor` came from the library, which stays loaded; every
        // pointer handed over outlives the UI (the feature store, the
        // controller and the strings are kept until cleanup; the UI copies
        // `plugin_uri`/`bundle_path` per the LV2 UI spec... and we keep the
        // feature array alive in `store` too).
        let instantiate = unsafe { (*descriptor).instantiate }
            .ok_or_else(|| PluginError::Editor("UI has no instantiate".into()))?;
        let handle = unsafe {
            instantiate(
                descriptor,
                plugin_uri.as_ptr(),
                bundle.as_ptr(),
                Some(write_function),
                controller.cast(),
                &mut widget,
                ptrs.as_ptr(),
            )
        };
        store._strings.push(plugin_uri);
        store._strings.push(bundle);
        if handle.is_null() {
            return Err(PluginError::Editor(format!(
                "{} failed to instantiate",
                info.uri
            )));
        }
        let external = match parent {
            Some(_) => {
                // SAFETY: the UI is not running right now.
                if let Some(x) = unsafe { (*controller).x11.as_mut() } {
                    x.child = widget as usize as u32;
                }
                None
            }
            None if widget.is_null() => {
                // SAFETY: instantiated above and not yet cleaned up.
                if let Some(cleanup) = unsafe { (*descriptor).cleanup } {
                    unsafe { cleanup(handle) };
                }
                return Err(PluginError::Editor("external UI returned no widget".into()));
            }
            None => Some(widget.cast::<ExternalWidget>()),
        };
        // The feature pointer array itself is only read during instantiate.
        drop(ptrs);
        Ok((descriptor, handle, store, external))
    }

    fn after_instantiate(&mut self) {
        // SAFETY: `descriptor` and `handle` are live.
        let ext = unsafe { (*self.descriptor).extension_data };
        let get = |uri: &[u8]| -> *const c_void {
            // SAFETY: `extension_data` takes a NUL-terminated URI.
            ext.map_or(std::ptr::null(), |f| unsafe { f(uri.as_ptr().cast()) })
        };
        let idle = get(lv2_sys::LV2_UI__idleInterface);
        if !idle.is_null() {
            self.idle = Some(idle.cast());
        }
        let resize = get(lv2_sys::LV2_UI__resize);
        if !resize.is_null() {
            self.ui_resize = Some(resize.cast());
        }
        // SAFETY: the controller is ours and the UI is not running right now.
        if let Some(x) = unsafe { (*self.controller).x11.as_mut() } {
            if !x.sized && x.child != 0 {
                // The UI did not ask for a size: use its widget's.
                let geometry = x.host.connection().get_geometry(x.child);
                if let Ok(Ok(g)) = geometry.map(|r| r.reply()) {
                    x.resize(u32::from(g.width), u32::from(g.height));
                }
            }
            x.host.show();
        }
        if let Some(w) = self.external {
            // SAFETY: a live external widget.
            if let Some(show) = unsafe { (*w).show } {
                unsafe { show(w) };
            }
        }
    }

    pub(crate) fn set_title(&mut self, title: &str) {
        // SAFETY: the controller is ours; the UI is not running right now.
        if let Some(x) = unsafe { (*self.controller).x11.as_ref() } {
            x.host.set_title(title);
        }
    }

    /// Whether the UI changed a value since the last call.
    pub(crate) fn take_state_changed(&mut self) -> bool {
        // SAFETY: the controller is ours; the UI is not running right now.
        std::mem::take(unsafe { &mut (*self.controller).changed })
    }

    /// Queues a message for the audio thread as if the UI wrote it.
    pub(crate) fn queue(&mut self, port: u32, kind: u8, body: &[u8]) -> bool {
        // SAFETY: the controller is ours; the UI is not running right now.
        ring::write(unsafe { &mut (*self.controller).to_dsp }, port, kind, body)
    }

    /// Feeds the UI and lets it run. Returns false once the user closed it.
    pub(crate) fn idle(&mut self, controls: &[(u32, f32)]) -> bool {
        // SAFETY: `descriptor` and `handle` are live.
        let port_event = unsafe { (*self.descriptor).port_event };
        if let Some(port_event) = port_event {
            while let Some(m) = ring::read(&mut self.from_dsp, &mut self.atom_buf) {
                if let (ring::ATOM, Some(len)) = (m.kind, m.len) {
                    // SAFETY: a whole atom of `len` bytes for an atom port.
                    unsafe {
                        port_event(
                            self.handle,
                            m.port,
                            len as u32,
                            self.event_transfer,
                            self.atom_buf.as_ptr().cast(),
                        )
                    };
                }
            }
            for &(port, value) in controls {
                // SAFETY: the controller is ours; the UI is not running right now, and the
                // borrow ends before it does.
                let last_sent = unsafe { &mut (*self.controller).last_sent };
                let known = last_sent.iter_mut().find(|(p, _)| *p == port);
                let changed = match known {
                    Some((_, v)) if v.to_bits() == value.to_bits() => false,
                    Some((_, v)) => {
                        *v = value;
                        true
                    }
                    None => {
                        last_sent.push((port, value));
                        true
                    }
                };
                if changed {
                    // SAFETY: protocol 0 is a single float.
                    unsafe { port_event(self.handle, port, 4, 0, (&value as *const f32).cast()) };
                }
            }
        }
        if let Some(idle) = self.idle {
            // SAFETY: the UI's own idle interface.
            if let Some(f) = unsafe { (*idle).idle }
                && unsafe { f(self.handle) } != 0
            {
                return false;
            }
        }
        if let Some(w) = self.external {
            // SAFETY: a live external widget.
            if let Some(run) = unsafe { (*w).run } {
                unsafe { run(w) };
            }
        }
        // SAFETY (for each dereference of the controller below): it is ours,
        // and no reference to it is held across a call into the UI.
        if unsafe { (*self.controller).closed } {
            return false;
        }
        while let Some(x) = unsafe { (*self.controller).x11.as_mut() } {
            let Some(ev) = x.host.poll() else {
                break;
            };
            let resized = match ev {
                WindowEvent::Closed => return false,
                // A fixed-size UI keeps its size even if the window manager
                // ignores the size hints.
                WindowEvent::Resized(..) if x.host.is_fixed() => None,
                WindowEvent::Resized(w, h) => {
                    if x.child != 0 {
                        let aux = ConfigureWindowAux::new().width(w).height(h);
                        let conn = x.host.connection();
                        let _ = conn.configure_window(x.child, &aux);
                        let _ = conn.flush();
                    }
                    Some((w, h))
                }
                // A fixed-size UI that resizes its own widget, without asking
                // through ui:resize: the window and its hints follow.
                WindowEvent::Other(XEvent::ConfigureNotify(e))
                    if x.host.is_fixed() && x.child != 0 && e.window == x.child =>
                {
                    x.resize(u32::from(e.width), u32::from(e.height));
                    None
                }
                WindowEvent::Other(_) => None,
            };
            // `x` is no longer used: the UI may call back into the controller.
            if let (Some((w, h)), Some(r)) = (resized, self.ui_resize) {
                // SAFETY: the UI's own resize interface. Its `handle` field is
                // unused; the spec passes the UI handle.
                if let Some(f) = unsafe { (*r).ui_resize } {
                    // Window sizes are at most 16384.
                    unsafe { f(self.handle, w as c_int, h as c_int) };
                }
            }
        }
        true
    }

    /// Destroys the UI and its window, handing back the ring ends.
    pub(crate) fn close(self) -> UiEnds {
        let mut this = std::mem::ManuallyDrop::new(self);
        this.destroy();
        // SAFETY: the UI is gone, so the controller is no longer shared.
        let c = unsafe { Box::from_raw(this.controller) };
        // SAFETY: each field is moved out exactly once; `this` is never
        // dropped, so nothing is dropped twice.
        unsafe {
            let from_dsp = std::ptr::read(&this.from_dsp);
            drop(std::ptr::read(&this.atom_buf));
            drop(std::ptr::read(&this._features));
            drop(std::ptr::read(&this._host_features));
            UiEnds {
                to_dsp: c.to_dsp,
                from_dsp,
            }
        }
    }

    fn destroy(&mut self) {
        // An external UI that called `ui_closed` is defunct: it only gets
        // `cleanup`, never `hide`.
        // SAFETY: the controller is ours; the UI is not running right now.
        let closed = unsafe { (*self.controller).closed };
        if let Some(w) = self.external.take()
            && !closed
        {
            // SAFETY: a live external widget.
            if let Some(hide) = unsafe { (*w).hide } {
                unsafe { hide(w) };
            }
        }
        if !self.handle.is_null() {
            // SAFETY: instantiated and not yet cleaned up.
            if let Some(cleanup) = unsafe { (*self.descriptor).cleanup } {
                unsafe { cleanup(self.handle) };
            }
            self.handle = std::ptr::null_mut();
        }
    }
}

impl Drop for Editor {
    fn drop(&mut self) {
        self.destroy();
        // SAFETY: the UI is gone; free the controller (and its X11 window).
        drop(unsafe { Box::from_raw(self.controller) });
    }
}
