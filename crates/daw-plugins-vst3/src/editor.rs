//! Plugin editors in a floating X11 top-level window (XWayland on Wayland sessions).

use std::sync::{Arc, Mutex};

use daw_plugin_window::HostWindow;
use vst3::Steinberg::Linux::IRunLoop;
use vst3::Steinberg::Vst::{IEditController, IEditControllerTrait, ViewType};
use vst3::Steinberg::{
    IPlugFrame, IPlugFrameTrait, IPlugView, IPlugViewTrait, ViewRect, kInvalidArgument,
    kPlatformTypeX11EmbedWindowID, kResultFalse, kResultOk, kResultTrue, tresult,
};
use vst3::{Class, ComPtr, ComWrapper};
use x11rb::connection::Connection;
use x11rb::protocol::Event;

use crate::host::host;
use crate::runloop::forward_run_loop;

/// The window the editor lives in. Shared with the frame so `resizeView` can resize it.
struct Window {
    host: HostWindow,
    resizable: bool,
}

impl Window {
    fn resize(&self, width: u32, height: u32) {
        self.host.resize(width, height, !self.resizable);
    }
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

        let window = Arc::new(Window {
            host: HostWindow::new(title, width, height, !resizable)?,
            resizable,
        });
        let id = window.host.id();

        let frame = ComWrapper::new(PlugFrame::default());
        *frame.window.lock().unwrap() = Some(window.clone());
        unsafe {
            let frame_ptr = frame
                .as_com_ref::<IPlugFrame>()
                .map_or(std::ptr::null_mut(), |r| r.as_ptr());
            view.setFrame(frame_ptr);
            if view.attached(id as usize as *mut _, kPlatformTypeX11EmbedWindowID) != kResultOk {
                view.setFrame(std::ptr::null_mut());
                // Drops the frame's reference, so the window goes with ours even if the
                // plugin still holds the frame.
                *frame.window.lock().unwrap() = None;
                return Err("editor failed to attach".into());
            }
        }
        window.host.show();

        Ok(Editor {
            view,
            frame,
            window,
        })
    }

    pub fn set_title(&self, title: &str) {
        self.window.host.set_title(title);
    }

    /// Handles window events and the plugin's run-loop callbacks. Returns false once the user
    /// has closed the window.
    pub fn pump(&mut self) -> bool {
        let mut open = true;
        let mut resized = None;
        let window = &self.window.host;
        while let Ok(Some(event)) = window.connection().poll_for_event() {
            match event {
                event if window.is_close(&event) => open = false,
                Event::ConfigureNotify(c) if c.window == window.id() => {
                    resized = Some((c.width as i32, c.height as i32));
                }
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
        // The window itself goes with `self.window`, after the view is removed.
        *self.frame.window.lock().unwrap() = None;
    }
}
