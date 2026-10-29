//! C++ glue for the KDE Frameworks calls that have no Rust binding:
//! application startup (KAboutData, KLocalizedString, KCrash, the QML
//! engine) and KConfig. See `cpp/kadence.h`.

#[cxx::bridge(namespace = "kadence")]
mod bridge {
    unsafe extern "C++" {
        include!("daw-app/src/cpp/kadence.h");

        /// Set up the KDE application and run its event loop.
        #[cxx_name = "runApplication"]
        fn run_application(args: &Vec<String>, version: &str) -> i32;

        /// Save the visible window titled `title` as an image. For tests.
        #[cxx_name = "saveWindowScreenshot"]
        fn save_window_screenshot(title: &str, path: &str) -> bool;

        /// A value from the app's KConfig file, or "" when unset.
        #[cxx_name = "configRead"]
        fn config_read(group: &str, key: &str) -> String;

        /// Store a value in the app's KConfig file and sync it to disk.
        #[cxx_name = "configWrite"]
        fn config_write(group: &str, key: &str, value: &str);
    }
}

pub use bridge::*;
