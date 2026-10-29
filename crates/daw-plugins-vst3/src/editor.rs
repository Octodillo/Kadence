//! Plugin editors in a floating X11 top-level window (XWayland on Wayland sessions).

use std::sync::{Arc, Mutex};

use vst3::Steinberg::Linux::IRunLoop;
use vst3::Steinberg::Vst::{IEditController, IEditControllerTrait, ViewType};
use vst3::Steinberg::{
    IPlugFrame, IPlugFrameTrait, IPlugView, IPlugViewTrait, ViewRect, kInvalidArgument,
    kPlatformTypeX11EmbedWindowID, kResultFalse, kResultOk, kResultTrue, tresult,
};
use vst3::{Class, ComPtr, ComWrapper};
use x11rb::connection::Connection;
use x11rb::properties::WmSizeHints;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{
    AtomEnum, ConfigureWindowAux, ConnectionExt, CreateWindowAux, EventMask, PropMode, WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

use crate::host::host;
use crate::runloop::forward_run_loop;

/// The window the editor lives in. Shared with the frame so `resizeView` can resize it.
struct Window {
    conn: RustConnection,
    id: u32,
    resizable: bool,
}

impl Window {
    fn resize(&self, width: u32, height: u32) {
        if !self.resizable {
            let _ = fixed_size_hints(width, height).set_normal_hints(&self.conn, self.id);
        }
        let _ = self.conn.configure_window(
            self.id,
            &ConfigureWindowAux::new().width(width).height(height),
        );
        let _ = self.conn.flush();
    }
}

fn fixed_size_hints(width: u32, height: u32) -> WmSizeHints {
    let mut hints = WmSizeHints::new();
    hints.min_size = Some((width as i32, height as i32));
    hints.max_size = Some((width as i32, height as i32));
    hints
}

fn rect_size(r: &ViewRect) -> (u32, u32) {
    (
        (r.right - r.left).max(1) as u32,
        (r.bottom - r.top).max(1) as u32,
    )
}

/// `IPlugFrame` plus `IRunLoop`: older plugins ask the frame for the run loop, newer ones the
/// host context. Both forward to the same process-wide loop.
#[derive(Default)]
struct PlugFrame {
    window: Mutex<Option<Arc<Window>>>,
}

impl Class for PlugFrame {
    type Interfaces = (IPlugFrame, IRunLoop);
}

impl IPlugFrameTrait for PlugFrame {
    unsafe fn resizeView(&self, view: *mut IPlugView, new_size: *mut ViewRect) -> tresult {
        if view.is_null() || new_size.is_null() {
            return kInvalidArgument;
        }
        let (w, h) = rect_size(unsafe { &*new_size });
        if let Some(window) = self.window.lock().unwrap().clone() {
            window.resize(w, h);
        }
        unsafe { ((*(*view).vtbl).onSize)(view, new_size) };
        kResultTrue
    }
}

forward_run_loop!(PlugFrame, self => host().run_loop);

pub struct Editor {
    view: ComPtr<IPlugView>,
    frame: ComWrapper<PlugFrame>,
    window: Arc<Window>,
    wm_protocols: u32,
    wm_delete: u32,
}

impl Editor {
    /// Whether the controller offers an X11 editor at all. Creates and drops a view.
    pub fn probe(controller: &ComPtr<IEditController>) -> bool {
        let Some(view) = (unsafe { ComPtr::from_raw(controller.createView(ViewType::kEditor)) })
        else {
            return false;
        };
        unsafe { view.isPlatformTypeSupported(kPlatformTypeX11EmbedWindowID) == kResultTrue }
    }

    pub fn open(controller: &ComPtr<IEditController>, title: &str) -> Result<Editor, String> {
        let view = unsafe { ComPtr::from_raw(controller.createView(ViewType::kEditor)) }
            .ok_or("plugin has no editor")?;
        unsafe {
            if view.isPlatformTypeSupported(kPlatformTypeX11EmbedWindowID) != kResultTrue {
                return Err("editor does not support X11".into());
            }
        }
        let mut rect = ViewRect {
            left: 0,
            top: 0,
            right: 400,
            bottom: 300,
        };
        let resizable = unsafe {
            view.getSize(&mut rect);
            view.canResize() == kResultTrue
        };
        let (width, height) = rect_size(&rect);

        let (conn, screen) = x11rb::connect(None).map_err(|e| format!("no X display: {e}"))?;
        let x = |e: x11rb::errors::ReplyOrIdError| e.to_string();
        let root = conn.setup().roots[screen].root;
        let black = conn.setup().roots[screen].black_pixel;
        let id = conn.generate_id().map_err(x)?;
        let atom = |name: &str| -> Result<u32, String> {
            Ok(conn
                .intern_atom(false, name.as_bytes())
                .map_err(|e| e.to_string())?
                .reply()
                .map_err(|e| e.to_string())?
                .atom)
        };
        let wm_protocols = atom("WM_PROTOCOLS")?;
        let wm_delete = atom("WM_DELETE_WINDOW")?;
        let net_wm_name = atom("_NET_WM_NAME")?;
        let utf8 = atom("UTF8_STRING")?;

        (|| -> Result<(), x11rb::errors::ConnectionError> {
            conn.create_window(
                x11rb::COPY_DEPTH_FROM_PARENT,
                id,
                root,
                0,
                0,
                width as u16,
                height as u16,
                0,
                WindowClass::INPUT_OUTPUT,
                0,
                &CreateWindowAux::new()
                    .background_pixel(black)
                    .event_mask(EventMask::STRUCTURE_NOTIFY),
            )?;
            conn.change_property8(
                PropMode::REPLACE,
                id,
                AtomEnum::WM_NAME,
                AtomEnum::STRING,
                title.as_bytes(),
            )?;
            conn.change_property8(PropMode::REPLACE, id, net_wm_name, utf8, title.as_bytes())?;
            conn.change_property8(
                PropMode::REPLACE,
                id,
                AtomEnum::WM_CLASS,
                AtomEnum::STRING,
                b"kadence\0Kadence\0",
            )?;
            conn.change_property32(
                PropMode::REPLACE,
                id,
                wm_protocols,
                AtomEnum::ATOM,
                &[wm_delete],
            )?;
            Ok(())
        })()
        .map_err(|e| e.to_string())?;

        let window = Arc::new(Window {
            conn,
            id,
            resizable,
        });
        if !resizable {
            let _ = fixed_size_hints(width, height).set_normal_hints(&window.conn, id);
        }

        // The plugin creates its child window on its own X connection, so ours must exist on
        // the server before `attached`, not just sit in our output buffer.
        window.conn.sync().map_err(|e| e.to_string())?;

        let frame = ComWrapper::new(PlugFrame::default());
        *frame.window.lock().unwrap() = Some(window.clone());
        unsafe {
            let frame_ptr = frame
                .as_com_ref::<IPlugFrame>()
                .map_or(std::ptr::null_mut(), |r| r.as_ptr());
            view.setFrame(frame_ptr);
            if view.attached(id as usize as *mut _, kPlatformTypeX11EmbedWindowID) != kResultOk {
                view.setFrame(std::ptr::null_mut());
                let _ = window.conn.destroy_window(id);
                let _ = window.conn.flush();
                return Err("editor failed to attach".into());
            }
        }
        let _ = window.conn.map_window(id);
        let _ = window.conn.flush();

        Ok(Editor {
            view,
            frame,
            window,
            wm_protocols,
            wm_delete,
        })
    }

    /// Handles window events and the plugin's run-loop callbacks. Returns false once the user
    /// has closed the window.
    pub fn pump(&mut self) -> bool {
        let mut open = true;
        let mut resized = None;
        while let Ok(Some(event)) = self.window.conn.poll_for_event() {
            match event {
                Event::ClientMessage(m)
                    if m.type_ == self.wm_protocols && m.data.as_data32()[0] == self.wm_delete =>
                {
                    open = false;
                }
                Event::ConfigureNotify(c) if c.window == self.window.id => {
                    resized = Some((c.width as i32, c.height as i32));
                }
                Event::DestroyNotify(d) if d.window == self.window.id => open = false,
                _ => {}
            }
        }
        if let (Some((w, h)), true) = (resized, self.window.resizable) {
            let mut current = ViewRect {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            };
            unsafe {
                self.view.getSize(&mut current);
                if rect_size(&current) != (w as u32, h as u32) {
                    let mut rect = ViewRect {
                        left: 0,
                        top: 0,
                        right: w,
                        bottom: h,
                    };
                    if self.view.checkSizeConstraint(&mut rect) != kResultFalse {
                        self.view.onSize(&mut rect);
                        let (cw, ch) = rect_size(&rect);
                        if (cw, ch) != (w as u32, h as u32) {
                            self.window.resize(cw, ch);
                        }
                    }
                }
            }
        }
        host().run_loop.pump();
        open
    }
}

impl Drop for Editor {
    fn drop(&mut self) {
        unsafe {
            self.view.removed();
            self.view.setFrame(std::ptr::null_mut());
        }
        *self.frame.window.lock().unwrap() = None;
        let _ = self.window.conn.destroy_window(self.window.id);
        let _ = self.window.conn.flush();
    }
}
