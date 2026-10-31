//! `PluginEditors`: opens and closes the plugins' own editor windows, for
//! the device's "Show Plugin Interface" action.
//!
//! The windows are the plugins' own top-level windows (see
//! [`AppState::open_plugin_editor`](crate::state::AppState::open_plugin_editor)),
//! titled after the device and its track. Devices are named by their id, as
//! `DevicePanel.deviceId` gives it.

use std::pin::Pin;

use cxx_qt::CxxQtType;
use cxx_qt_lib::QString;

use crate::state;

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
    }

    #[auto_cxx_name]
    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qml_singleton]
        /// Bumped when an editor opens or closes, from here or by the user
        /// closing its window; bind to it to keep a button's state fresh.
        #[qproperty(i32, revision)]
        type PluginEditors = super::PluginEditorsRust;

        /// Whether the device's plugin has an editor of its own. False for
        /// built-in devices and plugins that are not running.
        #[qinvokable]
        fn has_editor(self: &PluginEditors, device: &QString) -> bool;

        #[qinvokable]
        fn is_open(self: &PluginEditors, device: &QString) -> bool;

        /// Opens the editor, or leaves it open. Returns false, with the
        /// reason in the status line, if it would not open.
        #[qinvokable]
        fn show(self: Pin<&mut PluginEditors>, device: &QString) -> bool;

        #[qinvokable]
        fn close(self: Pin<&mut PluginEditors>, device: &QString);

        /// How windows are titled, in the user's language: `format` with
        /// `%1` for the device and `%2` for the track, and the master
        /// track's name.
        #[qinvokable]
        fn set_titles(self: Pin<&mut PluginEditors>, format: &QString, master: &QString);

        /// Picks up editors the user closed. Call it from a timer while a
        /// view shows editor state.
        #[qinvokable]
        fn refresh(self: Pin<&mut PluginEditors>);
    }
}

#[derive(Default)]
pub struct PluginEditorsRust {
    revision: i32,
    /// The open editors at the last refresh.
    open: Vec<daw_model::DeviceId>,
}

/// The device whose id reads `id`.
fn device(s: &state::AppState, id: &QString) -> Option<daw_model::DeviceId> {
    let id = id.to_string();
    s.doc
        .project()
        .all_tracks()
        .flat_map(|t| t.devices.iter())
        .find(|d| d.id.to_string() == id)
        .map(|d| d.id)
}

/// The plugin devices whose editor is open.
fn open_editors(s: &state::AppState) -> Vec<daw_model::DeviceId> {
    s.doc
        .project()
        .all_tracks()
        .flat_map(|t| t.devices.iter())
        .filter(|d| matches!(d.source, daw_model::DeviceSource::Plugin { .. }))
        .map(|d| d.id)
        .filter(|&d| s.plugin_editor_open(d))
        .collect()
}

impl qobject::PluginEditors {
    fn has_editor(&self, device: &QString) -> bool {
        state::with(|s| self::device(s, device).is_some_and(|d| s.plugin_has_editor(d)))
    }

    fn is_open(&self, device: &QString) -> bool {
        state::with(|s| self::device(s, device).is_some_and(|d| s.plugin_editor_open(d)))
    }

    fn show(mut self: Pin<&mut Self>, device: &QString) -> bool {
        let shown =
            state::with(|s| self::device(s, device).is_some_and(|d| s.open_plugin_editor(d)));
        self.as_mut().refresh();
        shown
    }

    fn close(mut self: Pin<&mut Self>, device: &QString) {
        state::with(|s| {
            if let Some(d) = self::device(s, device) {
                s.close_plugin_editor(d);
            }
        });
        self.as_mut().refresh();
    }

    fn set_titles(self: Pin<&mut Self>, format: &QString, master: &QString) {
        state::with(|s| {
            s.editor_title_format = format.to_string();
            s.master_name = master.to_string();
            s.retitle_editors();
        });
    }

    fn refresh(mut self: Pin<&mut Self>) {
        let open = state::with(|s| open_editors(s));
        if open != self.open {
            self.as_mut().rust_mut().open = open;
            let revision = self.revision().wrapping_add(1);
            self.set_revision(revision);
        }
    }
}
