//! The host window for plugins whose editor embeds into a parent X11 window, which is what
//! JUCE-based CLAPs (Surge XT among them) offer instead of a floating window. Under Wayland this
//! is an XWayland window.

use x11rb::connection::Connection;
use x11rb::properties::WmSizeHints;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{
    AtomEnum, ConfigureWindowAux, ConnectionExt as _, CreateWindowAux, EventMask, PropMode,
    WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

/// What happened to the window since the last [`HostWindow::pump`].
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct WindowEvents {
    pub(crate) closed: bool,
    pub(crate) resized: Option<(u32, u32)>,
}

/// A top-level X11 window that a plugin editor is embedded into.
pub(crate) struct HostWindow {
    conn: RustConnection,
    id: u32,
    wm_protocols: u32,
    wm_delete: u32,
    size: (u32, u32),
}

impl HostWindow {
    /// Creates (but does not map) a window of `width` x `height` physical pixels.
    pub(crate) fn new(
        title: &str,
        width: u32,
        height: u32,
        resizable: bool,
    ) -> Result<Self, String> {
        let (conn, screen) = x11rb::connect(None).map_err(|e| format!("no X display: {e}"))?;
        let err = |e: &dyn std::fmt::Display| e.to_string();
        let screen = &conn.setup().roots[screen];
        let (root, black) = (screen.root, screen.black_pixel);
        let atom = |name: &str| -> Result<u32, String> {
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
        (|| -> Result<(), x11rb::errors::ConnectionError> {
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
            // Lets the window manager group the editor with Kadence's own windows.
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
        let window = Self {
            conn,
            id,
            wm_protocols,
            wm_delete,
            size: (u32::from(w), u32::from(h)),
        };
        if !resizable {
            window.fix_size(w, h);
        }
        // The plugin creates its child on its own X connection, so the parent must exist on the
        // server before `set_parent`, not just sit in our output buffer.
        window.conn.sync().map_err(|e| err(&e))?;
        Ok(window)
    }

    pub(crate) fn id(&self) -> u32 {
        self.id
    }

    pub(crate) fn show(&self) {
        let _ = self.conn.map_window(self.id);
        let _ = self.conn.flush();
    }

    /// Resizes the window, for the plugin's own resize requests.
    pub(crate) fn resize(&mut self, width: u32, height: u32, resizable: bool) {
        let (w, h) = (clamp_dim(width), clamp_dim(height));
        if !resizable {
            self.fix_size(w, h);
        }
        let _ = self.conn.configure_window(
            self.id,
            &ConfigureWindowAux::new()
                .width(u32::from(w))
                .height(u32::from(h)),
        );
        let _ = self.conn.flush();
        self.size = (u32::from(w), u32::from(h));
    }

    /// Drains window events without blocking.
    pub(crate) fn pump(&mut self) -> WindowEvents {
        let mut events = WindowEvents::default();
        loop {
            let event = match self.conn.poll_for_event() {
                Ok(Some(event)) => event,
                Ok(None) => break,
                // A dead connection means the window is gone too.
                Err(_) => {
                    events.closed = true;
                    break;
                }
            };
            match event {
                Event::ClientMessage(m)
                    if m.type_ == self.wm_protocols && m.data.as_data32()[0] == self.wm_delete =>
                {
                    events.closed = true;
                }
                Event::DestroyNotify(d) if d.window == self.id => events.closed = true,
                Event::ConfigureNotify(c) if c.window == self.id => {
                    let size = (u32::from(c.width), u32::from(c.height));
                    if size != self.size {
                        self.size = size;
                        events.resized = Some(size);
                    }
                }
                _ => {}
            }
        }
        events
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

fn clamp_dim(v: u32) -> u16 {
    u16::try_from(v.clamp(1, 16_384)).unwrap_or(u16::MAX)
}
