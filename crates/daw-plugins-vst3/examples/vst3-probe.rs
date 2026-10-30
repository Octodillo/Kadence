//! Manual check against real plugins: scans a `.vst3` bundle (or a directory of them), then for
//! each plugin found it instantiates, processes audio and notes, moves a parameter, round-trips
//! state and optionally opens the editor.
//!
//! ```text
//! cargo run -p daw-plugins-vst3 --example vst3-probe -- "/usr/lib/vst3/Surge XT.vst3" --editor 10
//! ```

use std::path::PathBuf;
use std::time::{Duration, Instant};

use daw_core::{
    Event, EventKind, EventList, PluginFormat, PluginInstance, ProcessContext, SampleTime,
    Timeline, TransportState,
};
use daw_plugins_vst3::Vst3Format;

const SAMPLE_RATE: f64 = 48_000.0;
const FRAMES: u32 = 256;

fn block(p: &mut dyn PluginInstance, position: i64, events: &[Event]) -> (f32, usize) {
    let ports = p.ports();
    let input: Vec<f32> = (0..FRAMES)
        .map(|i| ((position + i as i64) as f32 * 0.05).sin() * 0.25)
        .collect();
    let ins: Vec<Vec<f32>> = (0..ports.audio_in).map(|_| input.clone()).collect();
    let mut outs: Vec<Vec<f32>> = (0..ports.audio_out)
        .map(|_| vec![0.0; FRAMES as usize])
        .collect();
    let mut events_in = EventList::with_capacity(64);
    for e in events {
        let _ = events_in.push(*e);
    }
    let mut events_out = EventList::with_capacity(256);
    let transport = TransportState::at(
        &Timeline::default(),
        true,
        SampleTime(position),
        SAMPLE_RATE,
    );
    let audio_in: Vec<&[f32]> = ins.iter().map(|v| v.as_slice()).collect();
    let mut audio_out: Vec<&mut [f32]> = outs.iter_mut().map(|v| v.as_mut_slice()).collect();
    p.process(&mut ProcessContext {
        frames: FRAMES,
        transport: &transport,
        audio_in: &audio_in,
        audio_out: &mut audio_out,
        events_in: &events_in,
        events_out: &mut events_out,
    });
    let peak = outs.iter().flatten().fold(0.0f32, |m, x| {
        if x.is_finite() {
            m.max(x.abs())
        } else {
            f32::NAN
        }
    });
    (peak, events_out.len())
}

fn note(on: bool) -> Event {
    let kind = if on {
        EventKind::NoteOn {
            channel: 0,
            key: 60,
            velocity: 0.8,
            note_id: -1,
        }
    } else {
        EventKind::NoteOff {
            channel: 0,
            key: Some(60),
            velocity: 0.0,
            note_id: -1,
        }
    };
    Event { frame: 0, kind }
}

fn run(p: &mut dyn PluginInstance, blocks: usize, first: &[Event], pos: &mut i64) -> f32 {
    let mut peak = 0.0f32;
    for b in 0..blocks {
        let (pk, _) = block(p, *pos, if b == 0 { first } else { &[] });
        peak = if pk.is_nan() { f32::NAN } else { peak.max(pk) };
        *pos += FRAMES as i64;
        p.on_main_thread();
    }
    peak
}

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next().map(PathBuf::from) else {
        eprintln!("usage: vst3-probe <bundle or dir> [--editor SECONDS]");
        std::process::exit(2);
    };
    let editor_secs: u64 = match (args.next().as_deref(), args.next()) {
        (Some("--editor"), Some(s)) => s.parse().unwrap_or(5),
        _ => 0,
    };

    let found = Vst3Format.scan(&path).unwrap_or_else(|e| {
        println!("scan failed: {e}");
        std::process::exit(1)
    });
    println!("scan: {} plugin(s) in {}", found.len(), path.display());
    let mut failures = 0;
    for d in &found {
        println!(
            "\n== {} by {} (instrument: {}, ports {:?}, id {})",
            d.name, d.vendor, d.is_instrument, d.ports, d.id
        );
        let mut p = match Vst3Format.instantiate(d, SAMPLE_RATE, FRAMES) {
            Ok(p) => p,
            Err(e) => {
                println!("  FAIL instantiate: {e}");
                failures += 1;
                continue;
            }
        };
        let ports = p.ports();
        let params = p.params();
        println!(
            "  ports {}in/{}out events in:{} out:{}, latency {}, {} params",
            ports.audio_in,
            ports.audio_out,
            ports.event_in,
            ports.event_out,
            p.latency(),
            params.len()
        );
        for info in params.iter().take(5) {
            println!(
                "    #{} {:?} [{} .. {}] default {} now {}",
                info.id,
                info.name,
                info.min,
                info.max,
                info.default,
                p.param_value(info.id)
            );
        }

        let mut pos = 0;
        let started = Instant::now();
        let peak = run(p.as_mut(), 40, &[note(true)], &mut pos);
        run(p.as_mut(), 4, &[note(false)], &mut pos);
        println!(
            "  process: peak {peak:.4} over 44 blocks in {:?}",
            started.elapsed()
        );
        if peak.is_nan() {
            println!("  FAIL non-finite output");
            failures += 1;
        } else if d.is_instrument && peak == 0.0 {
            println!("  WARN instrument stayed silent on a note");
        }

        if let Some(info) = params.iter().find(|i| i.automatable && i.max > i.min) {
            let target = info.min + (info.max - info.min) * 0.25;
            let ev = Event {
                frame: 0,
                kind: EventKind::ParamValue {
                    param: info.id,
                    value: target,
                },
            };
            run(p.as_mut(), 2, &[ev], &mut pos);
            let now = p.param_value(info.id);
            let ok = (now - target).abs() <= (info.max - info.min) * 0.02;
            println!(
                "  param {:?} -> {target:.4}: reads back {now:.4} {}",
                info.name,
                if ok { "ok" } else { "MISMATCH" }
            );
            if !ok {
                failures += 1;
            }
        }

        match p.save_state() {
            Ok(state) => match Vst3Format.instantiate(d, SAMPLE_RATE, FRAMES) {
                Ok(mut q) => match q.load_state(&state) {
                    Ok(()) => {
                        let again = q.save_state().map(|s| s.len()).unwrap_or(0);
                        println!(
                            "  state: {} bytes saved, restored into a new instance ({} bytes on re-save)",
                            state.len(),
                            again
                        );
                    }
                    Err(e) => {
                        println!("  FAIL load_state: {e}");
                        failures += 1;
                    }
                },
                Err(e) => println!("  FAIL second instance: {e}"),
            },
            Err(e) => {
                println!("  FAIL save_state: {e}");
                failures += 1;
            }
        }

        if editor_secs > 0 {
            if !p.has_editor() {
                println!("  editor: none offered");
            } else {
                match p.open_editor() {
                    Ok(()) => {
                        println!("  editor: open for {editor_secs}s (close it early to test that)");
                        let until = Instant::now() + Duration::from_secs(editor_secs);
                        while Instant::now() < until {
                            run(p.as_mut(), 1, &[], &mut pos);
                            std::thread::sleep(Duration::from_millis(16));
                        }
                        p.close_editor();
                        println!("  editor: closed cleanly");
                    }
                    Err(e) => {
                        println!("  FAIL open_editor: {e}");
                        failures += 1;
                    }
                }
            }
        }
        drop(p);
        println!("  dropped cleanly");
    }
    println!("\n{failures} failure(s)");
    std::process::exit(if failures == 0 && !found.is_empty() {
        0
    } else {
        1
    });
}
