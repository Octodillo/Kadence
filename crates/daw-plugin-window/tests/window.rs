//! The host window's properties as another X client (a window manager, xdotool) reads them.
//! Needs an X display; CI runs the tests under Xvfb.

use daw_plugin_window::HostWindow;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{Atom, AtomEnum, ConnectionExt as _};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

fn display() -> bool {
    let present = std::env::var_os("DISPLAY").is_some();
    if !present {
        eprintln!("skipped: no DISPLAY");
    }
    present
}

fn atom(conn: &RustConnection, name: &str) -> Atom {
    conn.intern_atom(false, name.as_bytes())
        .unwrap()
        .reply()
        .unwrap()
        .atom
}

/// A property's type and value, read on a connection of our own.
fn property(conn: &RustConnection, window: u32, property: impl Into<Atom>) -> (Atom, Vec<u8>) {
    let reply = conn
        .get_property(false, window, property, AtomEnum::ANY, 0, 1024)
        .unwrap()
        .reply()
        .unwrap();
    (reply.type_, reply.value)
}

#[test]
fn titles_are_utf8_in_both_properties() {
    if !display() {
        return;
    }
    let window = HostWindow::new("Surge XT — Keys", 300, 200, false).unwrap();
    let (conn, _) = x11rb::connect(None).unwrap();
    let utf8 = atom(&conn, "UTF8_STRING");
    let net_wm_name = atom(&conn, "_NET_WM_NAME");
    for title in ["Surge XT — Keys", "Surge XT — Bass"] {
        window.set_title(title);
        window.connection().sync().unwrap();
        for name in [AtomEnum::WM_NAME.into(), net_wm_name] {
            assert_eq!(
                property(&conn, window.id(), name),
                (utf8, title.as_bytes().to_vec()),
                "a UTF-8 title is typed UTF8_STRING, not Latin-1 STRING"
            );
        }
    }
    let (_, class) = property(&conn, window.id(), AtomEnum::WM_CLASS);
    assert_eq!(class, b"kadence\0Kadence\0");
    let (_, pid) = property(&conn, window.id(), atom(&conn, "_NET_WM_PID"));
    assert_eq!(pid, std::process::id().to_ne_bytes());
}

#[test]
fn a_fixed_window_pins_its_size_and_follows_resizes() {
    if !display() {
        return;
    }
    let window = HostWindow::new("Fixed", 300, 200, true).unwrap();
    let (conn, _) = x11rb::connect(None).unwrap();
    let hints = |conn: &RustConnection| {
        x11rb::properties::WmSizeHints::get_normal_hints(conn, window.id())
            .unwrap()
            .reply()
            .unwrap()
            .unwrap()
    };
    let h = hints(&conn);
    assert_eq!(
        (h.min_size, h.max_size),
        (Some((300, 200)), Some((300, 200)))
    );
    window.resize(640, 480, true);
    // Requests on two connections reach the server in no set order.
    window.connection().sync().unwrap();
    let h = hints(&conn);
    assert_eq!(
        (h.min_size, h.max_size),
        (Some((640, 480)), Some((640, 480)))
    );
    let g = conn.get_geometry(window.id()).unwrap().reply().unwrap();
    assert_eq!((g.width, g.height), (640, 480));
}

#[test]
fn destroying_the_window_reads_as_a_close() {
    if !display() {
        return;
    }
    let window = HostWindow::new("Closed", 300, 200, false).unwrap();
    let (conn, _) = x11rb::connect(None).unwrap();
    conn.destroy_window(window.id()).unwrap();
    conn.flush().unwrap();
    let mut closed = false;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !closed && std::time::Instant::now() < deadline {
        match window.connection().poll_for_event().unwrap() {
            Some(event) => closed |= window.is_close(&event),
            None => std::thread::sleep(std::time::Duration::from_millis(5)),
        }
    }
    assert!(closed, "DestroyNotify for the window is a close");
}
