//! The host window for plugins whose editor embeds into a parent X11 window, which is what
//! JUCE-based CLAPs (Surge XT among them) offer instead of a floating window.

use daw_plugin_window::HostWindow;
use x11rb::connection::Connection;
use x11rb::protocol::Event;

/// What happened to the window since the last [`EditorWindow::pump`].
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct WindowEvents {
    pub(crate) closed: bool,
    pub(crate) resized: Option<(u32, u32)>,
}

/// A [`HostWindow`] that remembers its size, so moving it is not reported as a resize.
pub(crate) struct EditorWindow {
    window: HostWindow,
    size: (u32, u32),
}

impl EditorWindow {
    /// Creates (but does not map) a window of `width` x `height` physical pixels.
    pub(crate) fn new(
        title: &str,
        width: u32,
        height: u32,
        resizable: bool,
    ) -> Result<Self, String> {
        Ok(Self {
            window: HostWindow::new(title, width, height, !resizable)?,
            size: (width, height),
        })
    }

    pub(crate) fn id(&self) -> u32 {
        self.window.id()
    }

    pub(crate) fn show(&self) {
        self.window.show();
    }

    pub(crate) fn set_title(&self, title: &str) {
        self.window.set_title(title);
    }

    /// Resizes the window, for the plugin's own resize requests.
    pub(crate) fn resize(&mut self, width: u32, height: u32, resizable: bool) {
        self.window.resize(width, height, !resizable);
        self.size = (width, height);
    }

    /// Drains window events without blocking.
    pub(crate) fn pump(&mut self) -> WindowEvents {
        let mut events = WindowEvents::default();
        loop {
            let event = match self.window.connection().poll_for_event() {
                Ok(Some(event)) => event,
                Ok(None) => break,
                // A dead connection means the window is gone too.
                Err(_) => {
                    events.closed = true;
                    break;
                }
            };
            match event {
                event if self.window.is_close(&event) => events.closed = true,
                Event::ConfigureNotify(c) if c.window == self.window.id() => {
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
}
