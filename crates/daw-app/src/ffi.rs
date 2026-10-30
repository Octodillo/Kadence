//! C++ glue for the KDE Frameworks calls that have no Rust binding:
//! application startup (KAboutData, KLocalizedString, KCrash, the QML
//! engine) and KConfig. See `cpp/kadence.h`.

#[cxx::bridge(namespace = "kadence")]
mod bridge {
    unsafe extern "C++" {
        include!("daw-app/src/cpp/kadence.h");

        /// Set up the KDE application and run its event loop. `args` holds
        /// the raw argument bytes, each followed by a NUL, since arguments
        /// (file names) need not be UTF-8.
        #[cxx_name = "runApplication"]
        fn run_application(args: &[u8], version: &str) -> i32;

        /// Save the visible window titled `title` as an image. For tests.
        #[cxx_name = "saveWindowScreenshot"]
        fn save_window_screenshot(title: &str, path: &str) -> bool;

        /// A value from the app's KConfig file, or "" when unset.
        #[cxx_name = "configRead"]
        fn config_read(group: &str, key: &str) -> String;

        /// Store a value in the app's KConfig file (in memory until synced).
        #[cxx_name = "configWrite"]
        fn config_write(group: &str, key: &str, value: &str);

        /// Write pending KConfig changes to disk.
        #[cxx_name = "configSync"]
        fn config_sync();
    }
}

pub use bridge::*;
