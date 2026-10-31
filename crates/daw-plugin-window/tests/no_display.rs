//! Without an X display the window fails with `NoDisplay`, which the app words as "plugin
//! interfaces need an X11 display". A test binary of its own, since it changes the environment.

use daw_core::plugin::PluginError;
use daw_plugin_window::HostWindow;

#[test]
fn no_display_is_its_own_error() {
    // SAFETY: the only test in this binary, so no other thread reads the environment.
    unsafe {
        std::env::remove_var("DISPLAY");
    }
    let err = HostWindow::new("Surge XT — Keys", 300, 200, false)
        .err()
        .expect("no window without a display");
    assert!(matches!(err, PluginError::NoDisplay(_)), "{err:?}");
}
