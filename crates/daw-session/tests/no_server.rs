//! With no audio server's socket in sight, the host does not open a client
//! (which can hang for good under pipewire-jack, #274): it runs silent and
//! says why. A file of its own, since it points the environment at an
//! empty folder for the whole process.

use daw_session::host::{AudioHost, HostConfig, HostKind};

#[test]
fn without_a_socket_the_host_runs_silent_without_opening_a_client() {
    let empty = std::env::temp_dir().join(format!("kadence-no-server-{}", std::process::id()));
    std::fs::create_dir_all(&empty).unwrap();
    // SAFETY: the only test in this process, and nothing else runs yet.
    unsafe {
        std::env::set_var("XDG_RUNTIME_DIR", &empty);
        std::env::set_var("PIPEWIRE_RUNTIME_DIR", &empty);
        std::env::remove_var("PIPEWIRE_REMOTE");
        std::env::remove_var("JACK_PROMISCUOUS_SERVER");
        std::env::set_var(
            "JACK_DEFAULT_SERVER",
            format!("kadence-none-{}", std::process::id()),
        );
    }
    assert!(!daw_backend::server_listening());

    let host = AudioHost::start(HostConfig::quiet("kadence-no-server-test"));
    assert_eq!(host.kind(), HostKind::Silent);
    let error = host.last_error().expect("says why it is silent");
    assert!(error.contains("no audio server"), "{error}");
    assert!(!host.server_appeared());
    drop(host);
    std::fs::remove_dir_all(&empty).unwrap();
}
