//! Opens an LV2 plugin's UI and keeps it running, for manual checks:
//! `cargo run -p daw-plugins-lv2 --example open_editor -- <plugin-uri> [seconds]`

use std::time::{Duration, Instant};

use daw_core::event::EventList;
use daw_core::node::{ProcessContext, TransportState};

fn main() {
    let mut args = std::env::args().skip(1);
    let uri = args
        .next()
        .expect("usage: open_editor <plugin-uri> [seconds]");
    let seconds: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(10);
    let format = daw_plugins_lv2::format();
    let d = format
        .default_search_paths()
        .iter()
        .flat_map(|dir| format.scan(dir).unwrap_or_default())
        .find(|d| d.id == uri)
        .expect("plugin not found");
    let mut p = format.instantiate(&d, 48_000.0, 512).expect("instantiate");
    p.prepare(48_000.0, 512);
    p.open_editor().expect("open editor");
    let ports = p.ports();
    let input = vec![vec![0.0f32; 512]; ports.audio_in as usize];
    let mut output = vec![vec![0.0f32; 512]; ports.audio_out as usize];
    let events_in = EventList::with_capacity(0);
    let mut events_out = EventList::with_capacity(64);
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(seconds) {
        let ins: Vec<&[f32]> = input.iter().map(|b| &b[..]).collect();
        let mut outs: Vec<&mut [f32]> = output.iter_mut().map(|b| &mut b[..]).collect();
        events_out.clear();
        p.process(&mut ProcessContext {
            frames: 512,
            transport: &TransportState::default(),
            audio_in: &ins,
            audio_out: &mut outs,
            events_in: &events_in,
            events_out: &mut events_out,
        });
        p.on_main_thread();
        std::thread::sleep(Duration::from_millis(1000 / 30));
    }
    p.close_editor();
}
