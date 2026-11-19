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

/// Whether a PipeWire or jackd socket for this user is there. Also `true`
/// when the environment names a PipeWire socket that cannot be looked for
/// as a file (an abstract socket, or a list): then only opening a client
/// tells.
pub fn server_listening() -> bool {
    // SAFETY: getuid has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    match candidates(&|name| env::var_os(name).map(PathBuf::from), uid) {
        Some(paths) => paths.iter().any(|path| is_socket(path)),
        None => true,
    }
}

fn is_socket(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.file_type().is_socket())
}

/// Where PipeWire looks for its socket when its runtime folder has none:
/// a server running for the whole system.
const PIPEWIRE_SYSTEM_DIR: &str = "/run/pipewire";

/// The sockets a server could listen on, from the environment (`var`) and
/// the user's id. `None` when the environment names a PipeWire socket that
/// is not a file to look for.
fn candidates(var: &dyn Fn(&str) -> Option<PathBuf>, uid: u32) -> Option<Vec<PathBuf>> {
    let mut paths = Vec::new();
    // PipeWire, which pipewire-jack talks to, looks where its own client
    // library does (`module-protocol-native/local-socket.c`):
    // `PIPEWIRE_REMOTE` names the socket (empty means the default), in the
    // runtime folder unless it is a path of its own, then in the system's.
    let remote = var("PIPEWIRE_REMOTE")
        .filter(|r| !r.as_os_str().is_empty())
        .unwrap_or_else(|| PathBuf::from("pipewire-0"));
    let text = remote.to_string_lossy();
    if text.starts_with('@') || text.starts_with('[') {
        // An abstract socket has no file; a list is PipeWire's to parse.
        return None;
    }
    if remote.is_absolute() {
        paths.push(remote);
    } else {
        if let Some(dir) = var("PIPEWIRE_RUNTIME_DIR").or_else(|| var("XDG_RUNTIME_DIR")) {
            paths.push(dir.join(&remote));
        }
        paths.push(Path::new(PIPEWIRE_SYSTEM_DIR).join(&remote));
    }
    // jackd keeps its sockets in /dev/shm: JACK2 as jack_<server>_<uid>_0,
    // JACK1 as jack_0 in jack-<uid>/<server>. A server shared between users
    // (JACK_PROMISCUOUS_SERVER) leaves the uid out of both.
    let server = var("JACK_DEFAULT_SERVER");
    let server = server
        .as_deref()
        .and_then(Path::to_str)
        .unwrap_or("default");
    paths.push(PathBuf::from(format!("/dev/shm/jack_{server}_{uid}_0")));
    paths.push(PathBuf::from(format!(
        "/dev/shm/jack-{uid}/{server}/jack_0"
    )));
    if var("JACK_PROMISCUOUS_SERVER").is_some() {
        paths.push(PathBuf::from(format!("/dev/shm/jack_{server}_0")));
        paths.push(PathBuf::from(format!("/dev/shm/jack/{server}/jack_0")));
    }
    Some(paths)
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
        let paths = candidates(&with(&[("XDG_RUNTIME_DIR", "/run/user/1000")]), 1000).unwrap();
        assert_eq!(
            paths,
            [
                PathBuf::from("/run/user/1000/pipewire-0"),
                PathBuf::from("/run/pipewire/pipewire-0"),
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
        )
        .unwrap();
        assert_eq!(paths[0], PathBuf::from("/tmp/pw/studio"));
        assert_eq!(paths[1], PathBuf::from("/run/pipewire/studio"));
        assert_eq!(paths[2], PathBuf::from("/dev/shm/jack_live_7_0"));
        let absolute = candidates(&with(&[("PIPEWIRE_REMOTE", "/srv/pw.sock")]), 7).unwrap();
        assert_eq!(absolute[0], PathBuf::from("/srv/pw.sock"));
        assert_ne!(absolute[1], PathBuf::from("/run/pipewire/srv/pw.sock"));
    }

    #[test]
    fn without_a_runtime_folder_pipewire_is_looked_for_system_wide() {
        let paths = candidates(&with(&[]), 7).unwrap();
        assert_eq!(paths[0], PathBuf::from("/run/pipewire/pipewire-0"));
    }

    #[test]
    fn an_empty_remote_is_the_default_one() {
        let paths = candidates(
            &with(&[("XDG_RUNTIME_DIR", "/run/user/7"), ("PIPEWIRE_REMOTE", "")]),
            7,
        )
        .unwrap();
        assert_eq!(paths[0], PathBuf::from("/run/user/7/pipewire-0"));
    }

    #[test]
    fn a_remote_that_is_no_file_cannot_be_looked_for() {
        assert_eq!(
            candidates(&with(&[("PIPEWIRE_REMOTE", "@pipewire-0")]), 7),
            None
        );
        let list = r#"["pipewire-0-manager", "pipewire-0"]"#;
        assert_eq!(candidates(&with(&[("PIPEWIRE_REMOTE", list)]), 7), None);
    }

    #[test]
    fn a_shared_jack_server_leaves_the_uid_out() {
        let paths = candidates(&with(&[("JACK_PROMISCUOUS_SERVER", "audio")]), 7).unwrap();
        assert!(paths.contains(&PathBuf::from("/dev/shm/jack_default_0")));
        assert!(paths.contains(&PathBuf::from("/dev/shm/jack/default/jack_0")));
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
