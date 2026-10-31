//! The top-level X11 window that plugin editors embed into, shared by the CLAP, VST3 and LV2
//! hosts. Plugins with an embeddable editor (JUCE-based ones such as Surge XT among them) take
//! a parent window instead of opening their own; under Wayland this is an XWayland window.
//!
//! The window carries Kadence's `WM_CLASS` and process id, so the window manager groups it
//! with Kadence's own windows, and asks for `WM_DELETE_WINDOW` so closing it is an event, not
//! a lost connection. Each format keeps its own event handling and only asks
//! [`HostWindow::is_close`] whether an event closes the window.

use x11rb::connection::Connection;
use x11rb::errors::ConnectionError;
use x11rb::properties::WmSizeHints;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ConfigureWindowAux, ConnectionExt as _, CreateWindowAux, EventMask, PropMode,
    Window, WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

const WM_CLASS: &[u8] = b"kadence\0Kadence\0";

/// A top-level X11 window on its own connection. Destroyed on drop.
pub struct HostWindow {
    conn: RustConnection,
    id: Window,
    wm_protocols: Atom,
    wm_delete: Atom,
    net_wm_name: Atom,
    utf8: Atom,
}

impl HostWindow {
    /// Creates (but does not map) a window of `width` x `height` physical pixels titled
    /// `title`. A `fixed` window gets size hints that pin it to that size.
    ///
    /// Besides its own window events the window reports its children's (substructure), so a
    /// host hears an embedded editor resize itself.
    pub fn new(title: &str, width: u32, height: u32, fixed: bool) -> Result<Self, String> {
        let (conn, screen) = x11rb::connect(None).map_err(|e| format!("no X display: {e}"))?;
        let err = |e: &dyn std::fmt::Display| e.to_string();
        let screen = &conn.setup().roots[screen];
        let (root, black) = (screen.root, screen.black_pixel);
        let atom = |name: &str| -> Result<Atom, String> {
            Ok(conn
                .intern_atom(false, name.as_bytes())
                .map_err(|e| err(&e))?
                .reply()
                .map_err(|e| err(&e))?
                .atom)
        };
        let wm_protocols = atom("WM_PROTOCOLS")?;
        let wm_delete = atom("WM_DELETE_WINDOW")?;
        let net_wm_name = atom("_NET_WM_NAME")?;
        let utf8 = atom("UTF8_STRING")?;
        let net_wm_pid = atom("_NET_WM_PID")?;
        let id = conn.generate_id().map_err(|e| err(&e))?;
        let (w, h) = (clamp_dim(width), clamp_dim(height));
        conn.create_window(
            x11rb::COPY_DEPTH_FROM_PARENT,
            id,
            root,
            0,
            0,
            w,
            h,
            0,
            WindowClass::INPUT_OUTPUT,
            x11rb::COPY_FROM_PARENT,
            &CreateWindowAux::new()
                .background_pixel(black)
                .event_mask(EventMask::STRUCTURE_NOTIFY | EventMask::SUBSTRUCTURE_NOTIFY),
        )
        .map_err(|e| err(&e))?;
        let window = Self {
            conn,
            id,
            wm_protocols,
            wm_delete,
            net_wm_name,
            utf8,
        };
        (|| -> Result<(), ConnectionError> {
            window.write_title(title)?;
            let conn = &window.conn;
            conn.change_property8(
                PropMode::REPLACE,
                id,
                AtomEnum::WM_CLASS,
                AtomEnum::STRING,
                WM_CLASS,
            )?;
            conn.change_property32(
                PropMode::REPLACE,
                id,
                wm_protocols,
                AtomEnum::ATOM,
                &[wm_delete],
            )?;
            conn.change_property32(
                PropMode::REPLACE,
                id,
                net_wm_pid,
                AtomEnum::CARDINAL,
                &[std::process::id()],
            )?;
            Ok(())
        })()
        .map_err(|e| err(&e))?;
        if fixed {
            window.fix_size(w, h);
        }
        // Plugins create their child on their own X connection, so the parent must exist on
        // the server before they get its id, not just sit in our output buffer.
        window.conn.sync().map_err(|e| err(&e))?;
        Ok(window)
    }

    /// The window's id, to hand to the plugin as its parent.
    pub fn id(&self) -> Window {
        self.id
    }

    /// The window's connection, for reading its events and for requests about the plugin's
    /// child window.
    pub fn connection(&self) -> &RustConnection {
        &self.conn
    }

    /// Maps the window.
    pub fn show(&self) {
        let _ = self.conn.map_window(self.id);
        let _ = self.conn.flush();
    }

    pub fn set_title(&self, title: &str) {
        let _ = self.write_title(title);
        let _ = self.conn.flush();
    }

    /// Resizes the window to `width` x `height` physical pixels. A `fixed` window's size hints
    /// follow, so the window manager offers no resize handles.
    pub fn resize(&self, width: u32, height: u32, fixed: bool) {
        let (w, h) = (clamp_dim(width), clamp_dim(height));
        if fixed {
            self.fix_size(w, h);
        }
        let _ = self.conn.configure_window(
            self.id,
            &ConfigureWindowAux::new()
                .width(u32::from(w))
                .height(u32::from(h)),
        );
        let _ = self.conn.flush();
    }

    /// Whether `event` means the window was closed: the user asked the window manager to
    /// close it, or it was destroyed.
    pub fn is_close(&self, event: &Event) -> bool {
        match event {
            Event::ClientMessage(m) => {
                m.window == self.id
                    && m.type_ == self.wm_protocols
                    && m.data.as_data32()[0] == self.wm_delete
            }
            Event::DestroyNotify(d) => d.window == self.id,
            _ => false,
        }
    }

    /// Writes both title properties. `WM_NAME` gets the same UTF-8 as `_NET_WM_NAME`, typed
    /// `UTF8_STRING` as Qt does: typed `STRING` it would be read as Latin-1, so a title with an
    /// em dash would show as mojibake in tools and window managers that read `WM_NAME`.
    fn write_title(&self, title: &str) -> Result<(), ConnectionError> {
        for property in [AtomEnum::WM_NAME.into(), self.net_wm_name] {
            self.conn.change_property8(
                PropMode::REPLACE,
                self.id,
                property,
                self.utf8,
                title.as_bytes(),
            )?;
        }
        Ok(())
    }

    fn fix_size(&self, w: u16, h: u16) {
        let mut hints = WmSizeHints::new();
        let (w, h) = (i32::from(w), i32::from(h));
        hints.min_size = Some((w, h));
        hints.max_size = Some((w, h));
        let _ = hints.set_normal_hints(&self.conn, self.id);
    }
}

impl Drop for HostWindow {
    fn drop(&mut self) {
        let _ = self.conn.destroy_window(self.id);
        let _ = self.conn.flush();
    }
}

/// X11 sizes are 16-bit and may not be zero; plugins sometimes ask for either.
fn clamp_dim(v: u32) -> u16 {
    u16::try_from(v.clamp(1, 16_384)).unwrap_or(u16::MAX)
}
