//! Whether an audio server that JACK clients reach seems to be running,
//! judged by its socket alone.
//!
//! Opening a client is the real test, but it is not a cheap one to repeat:
//! with pipewire-jack and no PipeWire running, the failed open can hang for
//! good while closing its half-made client (in `pw_thread_loop_stop`), and
//! it holds the jack crate's lock on client creation while it does, so every
//! later open hangs too. Looking for the socket never blocks. A socket left
//! behind by a server that died also counts: opening a client then tells.

use std::env;
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};

/// Whether a PipeWire or jackd socket for this user is there.
pub fn server_listening() -> bool {
    // SAFETY: getuid has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    candidates(&|name| env::var_os(name).map(PathBuf::from), uid)
        .iter()
        .any(|path| is_socket(path))
}

fn is_socket(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.file_type().is_socket())
}

/// The sockets a server could listen on, from the environment (`var`) and
/// the user's id.
fn candidates(var: &dyn Fn(&str) -> Option<PathBuf>, uid: u32) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    // PipeWire, which pipewire-jack talks to: `PIPEWIRE_REMOTE` names the
    // socket, in the runtime directory unless it is a path of its own.
    let remote = var("PIPEWIRE_REMOTE").unwrap_or_else(|| PathBuf::from("pipewire-0"));
    if remote.is_absolute() {
        paths.push(remote);
    } else if let Some(dir) = var("PIPEWIRE_RUNTIME_DIR").or_else(|| var("XDG_RUNTIME_DIR")) {
        paths.push(dir.join(remote));
    }
    // jackd: JACK2 listens on /dev/shm/jack_<server>_<uid>_0, JACK1 in a
    // folder of the user's.
    let server = var("JACK_DEFAULT_SERVER");
    let server = server
        .as_deref()
        .and_then(Path::to_str)
        .unwrap_or("default");
    paths.push(PathBuf::from(format!("/dev/shm/jack_{server}_{uid}_0")));
    paths.push(PathBuf::from(format!(
        "/dev/shm/jack-{uid}/{server}/jack_0"
    )));
    paths
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::os::unix::net::UnixListener;

    use super::*;

    fn with(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<PathBuf> {
        let vars: HashMap<String, PathBuf> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), PathBuf::from(v)))
            .collect();
        move |name| vars.get(name).cloned()
    }

    #[test]
    fn looks_where_pipewire_and_jackd_listen() {
        let paths = candidates(&with(&[("XDG_RUNTIME_DIR", "/run/user/1000")]), 1000);
        assert_eq!(
            paths,
            [
                PathBuf::from("/run/user/1000/pipewire-0"),
                PathBuf::from("/dev/shm/jack_default_1000_0"),
                PathBuf::from("/dev/shm/jack-1000/default/jack_0"),
            ]
        );
    }

    #[test]
    fn follows_the_names_the_environment_gives() {
        let paths = candidates(
            &with(&[
                ("PIPEWIRE_RUNTIME_DIR", "/tmp/pw"),
                ("XDG_RUNTIME_DIR", "/run/user/7"),
                ("PIPEWIRE_REMOTE", "studio"),
                ("JACK_DEFAULT_SERVER", "live"),
            ]),
            7,
        );
        assert_eq!(paths[0], PathBuf::from("/tmp/pw/studio"));
        assert_eq!(paths[1], PathBuf::from("/dev/shm/jack_live_7_0"));
        let absolute = candidates(&with(&[("PIPEWIRE_REMOTE", "/srv/pw.sock")]), 7);
        assert_eq!(absolute[0], PathBuf::from("/srv/pw.sock"));
    }

    #[test]
    fn only_a_socket_counts() {
        let dir = std::env::temp_dir().join(format!("kadence-listening-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("pipewire-0");
        std::fs::write(&file, b"").unwrap();
        assert!(!is_socket(&file), "a plain file is no server");
        std::fs::remove_file(&file).unwrap();
        assert!(!is_socket(&file), "nothing there");
        let _listener = UnixListener::bind(&file).unwrap();
        assert!(is_socket(&file));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
