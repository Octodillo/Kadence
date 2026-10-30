//! Tests against real LV2 plugins: mda-lv2 and x42-plugins.
//!
//! When the plugins are not installed the tests skip, unless
//! `DAW_REQUIRE_LV2_PLUGINS` is set (CI sets it), in which case they fail.

use std::path::{Path, PathBuf};

use assert_no_alloc::{AllocDisabler, assert_no_alloc, reset_violation_count, violation_count};
use daw_core::event::{Event, EventKind, EventList};
use daw_core::node::{Node, ProcessContext, TransportState};
use daw_core::plugin::{PluginDescriptor, PluginFormat, PluginInstance};

#[cfg(debug_assertions)]
#[global_allocator]
static A: AllocDisabler = AllocDisabler;

const SR: f64 = 48_000.0;
const BLOCK: u32 = 256;
const MDA: &str = "http://drobilla.net/plugins/mda/";

/// Xlib is not thread-safe, and real hosts run every UI on one thread, so the
/// editor tests take turns.
static EDITORS: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn lv2_dir() -> Option<PathBuf> {
    let dirs = daw_plugins_lv2::format().default_search_paths();
    let found = dirs
        .into_iter()
        .find(|d| d.join("mda.lv2").is_dir() && d.join("midimap.lv2").is_dir());
    if found.is_none() && std::env::var_os("DAW_REQUIRE_LV2_PLUGINS").is_some() {
        panic!("mda-lv2 and x42-plugins must be installed");
    }
    found
}

macro_rules! need_plugins {
    () => {
        match lv2_dir() {
            Some(d) => d,
            None => {
                eprintln!("skipped: mda-lv2 / x42-plugins not installed");
                return;
            }
        }
    };
}

fn find(format: &dyn PluginFormat, dir: &Path, bundle: &str, id: &str) -> PluginDescriptor {
    format
        .scan(&dir.join(bundle))
        .unwrap()
        .into_iter()
        .find(|d| d.id == id)
        .unwrap_or_else(|| panic!("{id} not found"))
}

fn load(dir: &Path, bundle: &str, id: &str) -> Box<dyn PluginInstance> {
    let format = daw_plugins_lv2::format();
    let d = find(&*format, dir, bundle, id);
    let mut p = format.instantiate(&d, SR, BLOCK).unwrap();
    p.prepare(SR, BLOCK);
    p
}

/// Runs one block. Returns the audio outputs and the output events.
fn run(
    p: &mut dyn PluginInstance,
    input: &[Vec<f32>],
    events: &[Event],
    transport: &TransportState,
) -> (Vec<Vec<f32>>, Vec<Event>) {
    let ports = p.ports();
    let mut outs = vec![vec![0.0f32; BLOCK as usize]; ports.audio_out as usize];
    let mut events_in = EventList::with_capacity(256);
    for e in events {
        events_in.push(*e).unwrap();
    }
    let mut events_out = EventList::with_capacity(64);
    {
        let ins: Vec<&[f32]> = input.iter().map(|b| &b[..]).collect();
        let mut out_refs: Vec<&mut [f32]> = outs.iter_mut().map(|b| &mut b[..]).collect();
        let mut ctx = ProcessContext {
            frames: BLOCK,
            transport,
            audio_in: &ins,
            audio_out: &mut out_refs,
            events_in: &events_in,
            events_out: &mut events_out,
        };
        // The workspace's `warn_debug` makes violations count (per thread)
        // instead of aborting.
        reset_violation_count();
        assert_no_alloc(|| p.process(&mut ctx));
        assert_eq!(violation_count(), 0, "process allocated");
    }
    (outs, events_out.iter().copied().collect())
}

fn silence(n: usize) -> Vec<Vec<f32>> {
    vec![vec![0.0; BLOCK as usize]; n]
}

fn peak(b: &[f32]) -> f32 {
    b.iter().fold(0.0, |m, x| m.max(x.abs()))
}

fn note_on(frame: u32, key: u8) -> Event {
    Event {
        frame,
        kind: EventKind::NoteOn {
            channel: 0,
            key,
            velocity: 1.0,
            note_id: -1,
        },
    }
}

fn param(frame: u32, param: u32, value: f64) -> Event {
    Event {
        frame,
        kind: EventKind::ParamValue { param, value },
    }
}

fn param_id(p: &dyn PluginInstance, name: &str) -> u32 {
    p.params()
        .iter()
        .find(|i| i.name == name)
        .unwrap_or_else(|| panic!("no param {name}"))
        .id
}

#[test]
fn scan_describes_plugins() {
    let dir = need_plugins!();
    let format = daw_plugins_lv2::format();
    let found = format.scan(&dir.join("mda.lv2")).unwrap();
    assert!(found.len() >= 30, "only {} mda plugins", found.len());
    let piano = found
        .iter()
        .find(|d| d.id == format!("{MDA}EPiano"))
        .unwrap();
    assert!(piano.is_instrument);
    assert_eq!(piano.name, "MDA ePiano");
    assert_eq!(piano.ports, (0, 2));
    assert_eq!(piano.path, dir.join("mda.lv2/"));
    let delay = found
        .iter()
        .find(|d| d.id == format!("{MDA}Delay"))
        .unwrap();
    assert!(!delay.is_instrument);
    assert_eq!(delay.ports, (2, 2));
    // Scanning the parent directory finds the same bundles and more.
    assert!(format.scan(&dir).unwrap().len() > found.len());
}

#[test]
fn an_instrument_plays_notes_without_allocating() {
    let dir = need_plugins!();
    let mut p = load(&dir, "mda.lv2", &format!("{MDA}EPiano"));
    assert!(p.ports().event_in);
    let t = TransportState::default();
    let (quiet, _) = run(&mut *p, &[], &[], &t);
    assert_eq!(peak(&quiet[0]), 0.0);
    let (out, _) = run(&mut *p, &[], &[note_on(128, 60)], &t);
    assert_eq!(peak(&out[0][..128]), 0.0, "sound before the note");
    assert!(peak(&out[0][128..]) > 0.01, "no sound after the note");
}

#[test]
fn automation_is_sample_accurate() {
    let dir = need_plugins!();
    let mut p = load(&dir, "mda.lv2", &format!("{MDA}Delay"));
    let mix = param_id(&*p, "FX Mix");
    let fb = param_id(&*p, "Feedback");
    let out_level = param_id(&*p, "Output");
    let t = TransportState::default();
    // Dry signal only, at full output.
    run(
        &mut *p,
        &silence(2),
        &[
            param(0, mix, 0.0),
            param(0, fb, 0.0),
            param(0, out_level, 1.0),
        ],
        &t,
    );
    assert_eq!(p.param_value(mix), 0.0);
    let input = vec![vec![0.25f32; BLOCK as usize]; 2];
    let (out, _) = run(&mut *p, &input, &[param(100, out_level, 0.0)], &t);
    assert!(
        out[0][..100].iter().all(|&x| x.abs() > 0.1),
        "output muted early"
    );
    assert!(
        out[0][100..].iter().all(|&x| x == 0.0),
        "output not muted at frame 100"
    );
    assert_eq!(p.param_value(out_level), 0.0);
}

#[test]
fn state_round_trips_into_a_new_instance() {
    let dir = need_plugins!();
    let id = format!("{MDA}Delay");
    let mut a = load(&dir, "mda.lv2", &id);
    let fb = param_id(&*a, "Feedback");
    let t = TransportState::default();
    run(&mut *a, &silence(2), &[param(0, fb, 0.123)], &t);
    let state = a.save_state().unwrap();

    let mut b = load(&dir, "mda.lv2", &id);
    assert_ne!(b.param_value(fb), a.param_value(fb));
    b.load_state(&state).unwrap();
    assert_eq!(b.param_value(fb), a.param_value(fb));
    assert!(b.load_state(b"not a state").is_err());
}

#[test]
fn reinstantiating_for_a_new_rate_keeps_state() {
    let dir = need_plugins!();
    let mut p = load(&dir, "mda.lv2", &format!("{MDA}Delay"));
    let fb = param_id(&*p, "Feedback");
    run(
        &mut *p,
        &silence(2),
        &[param(0, fb, 0.42)],
        &TransportState::default(),
    );
    let before = p.param_value(fb);
    p.prepare(44_100.0, BLOCK * 2);
    assert_eq!(p.param_value(fb), before);
    run(&mut *p, &silence(2), &[], &TransportState::default());
}

/// x42 midimap loads its mapping file in a worker, and remembers the file
/// in its state.
#[test]
fn worker_and_plugin_state() {
    let dir = need_plugins!();
    let tmp = tempfile::tempdir().unwrap();
    let cfg = tmp.path().join("map.cfg");
    std::fs::write(&cfg, "midimap v1\nNOTE 64 ANY | SAME 72 SAME\n").unwrap();

    let format = daw_plugins_lv2::Lv2Format::new();
    let d = find(
        &format,
        &dir,
        "midimap.lv2",
        "http://gareus.org/oss/lv2/midimap",
    );
    let mut p = format.instantiate_lv2(&d, SR).unwrap();
    assert!(p.ports().event_out);
    let t = TransportState::default();

    let msg = patch_set_path(&p, "http://gareus.org/oss/lv2/midimap#cfgfile", &cfg);
    // Too big to ever fit the port's buffer next to host events: refused.
    assert!(!p.queue_atom(0, &vec![0u8; 20_000]));
    assert!(!p.queue_atom(99, &msg), "no such atom input");
    assert!(p.queue_atom(0, &msg));
    let mut mapped = false;
    for _ in 0..50 {
        run(&mut p, &[], &[], &t);
        p.on_main_thread();
        let (_, out) = run(&mut p, &[], &[note_on(10, 64)], &t);
        if out
            .iter()
            .any(|e| matches!(e.kind, EventKind::NoteOn { key: 72, .. }))
        {
            assert_eq!(out[0].frame, 10);
            mapped = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(mapped, "worker never loaded the map");

    let state = p.save_state().unwrap();
    // midimap keeps the parsed map itself in its state, not the file name.
    std::fs::remove_file(&cfg).unwrap();
    let mut q = format.instantiate_lv2(&d, SR).unwrap();
    q.load_state(&state).unwrap();
    let mut mapped = false;
    for _ in 0..50 {
        q.on_main_thread();
        let (_, out) = run(&mut q, &[], &[note_on(0, 64)], &t);
        if out
            .iter()
            .any(|e| matches!(e.kind, EventKind::NoteOn { key: 72, .. }))
        {
            mapped = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(mapped, "restored state did not reload the map");
}

/// A `patch:Set` of a path property, as a whole atom.
fn patch_set_path(p: &daw_plugins_lv2::Lv2Instance, property: &str, path: &Path) -> Vec<u8> {
    let u = |s: &str| p.urid(s).to_ne_bytes();
    let mut path_bytes = path.to_str().unwrap().as_bytes().to_vec();
    path_bytes.push(0);
    let mut body = Vec::new();
    body.extend(0u32.to_ne_bytes()); // id
    body.extend(u("http://lv2plug.in/ns/ext/patch#Set"));
    // patch:property <property>
    body.extend(u("http://lv2plug.in/ns/ext/patch#property"));
    body.extend(0u32.to_ne_bytes());
    body.extend(4u32.to_ne_bytes());
    body.extend(u("http://lv2plug.in/ns/ext/atom#URID"));
    body.extend(u(property));
    body.extend(0u32.to_ne_bytes()); // pad
    // patch:value "<path>"
    body.extend(u("http://lv2plug.in/ns/ext/patch#value"));
    body.extend(0u32.to_ne_bytes());
    body.extend((path_bytes.len() as u32).to_ne_bytes());
    body.extend(u("http://lv2plug.in/ns/ext/atom#Path"));
    body.extend(&path_bytes);
    while body.len() % 8 != 0 {
        body.push(0);
    }
    let mut atom = Vec::new();
    atom.extend((body.len() as u32).to_ne_bytes());
    atom.extend(u("http://lv2plug.in/ns/ext/atom#Object"));
    atom.extend(body);
    atom
}

#[test]
fn latency_is_reported() {
    let dir = need_plugins!();
    let p = load(&dir, "dpl.lv2", "http://gareus.org/oss/lv2/dpl#stereo");
    assert!(p.latency() > 0, "dpl reports no latency");
    assert!(!p.params().iter().any(|i| i.name.contains("atency")));
}

#[test]
fn reset_silences_an_instrument() {
    let dir = need_plugins!();
    let mut p = load(&dir, "mda.lv2", &format!("{MDA}EPiano"));
    let t = TransportState::default();
    run(&mut *p, &[], &[note_on(0, 60)], &t);
    p.reset();
    let mut last = 1.0;
    for _ in 0..200 {
        last = peak(&run(&mut *p, &[], &[], &t).0[0]);
    }
    assert!(last < 1e-3, "still sounding after reset: {last}");
}

#[test]
fn editors_open_as_floating_windows() {
    let dir = need_plugins!();
    let mda = load(&dir, "mda.lv2", &format!("{MDA}Delay"));
    assert!(!mda.has_editor());
    let mut p = load(&dir, "balance.lv2", "http://gareus.org/oss/lv2/balance");
    assert!(p.has_editor());
    if std::env::var_os("DISPLAY").is_none() {
        eprintln!("skipped opening the editor: no DISPLAY");
        return;
    }
    let _turn = EDITORS.lock().unwrap_or_else(|e| e.into_inner());
    p.open_editor().unwrap();
    for _ in 0..20 {
        run(&mut *p, &silence(2), &[], &TransportState::default());
        p.on_main_thread();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    p.close_editor();
    // And again, to make sure the UI library survives being reopened.
    p.open_editor().unwrap();
    p.on_main_thread();
    drop(p);
}

/// Bigger toolkit UIs, when installed and a display is available.
#[test]
fn other_editors_open_when_available() {
    let dir = need_plugins!();
    if std::env::var_os("DISPLAY").is_none() {
        eprintln!("skipped: no DISPLAY");
        return;
    }
    let _turn = EDITORS.lock().unwrap_or_else(|e| e.into_inner());
    for (bundle, id) in [
        ("meters.lv2", "http://gareus.org/oss/lv2/meters#EBUr128"),
        (
            "lsp-plugins.lv2",
            "http://lsp-plug.in/plugins/lv2/para_equalizer_x8_stereo",
        ),
    ] {
        if !dir.join(bundle).is_dir() {
            continue;
        }
        let mut p = load(&dir, bundle, id);
        assert!(p.has_editor(), "{id} has no editor");
        p.open_editor().unwrap_or_else(|e| panic!("{id}: {e}"));
        let input = silence(p.ports().audio_in as usize);
        for _ in 0..10 {
            run(&mut *p, &input, &[], &TransportState::default());
            p.on_main_thread();
        }
        p.close_editor();
    }
}

/// A host block longer than the plugin was prepared for is run in pieces,
/// with every frame written and events kept on their own frames.
#[test]
fn long_blocks_are_processed_in_pieces() {
    let dir = need_plugins!();
    let mut p = load(&dir, "mda.lv2", &format!("{MDA}Delay"));
    let mix = param_id(&*p, "FX Mix");
    let fb = param_id(&*p, "Feedback");
    let out_level = param_id(&*p, "Output");
    // Longer than the 8192 frames plugins are told to expect, and with one
    // output buffer for the plugin's two, so the second goes to scratch.
    let frames = 8192 * 2 + 40;
    let input = vec![vec![0.25f32; frames]; 2];
    let mut outs = vec![vec![f32::NAN; frames]; 1];
    let mut events_in = EventList::with_capacity(8);
    for e in [
        param(0, mix, 0.0),
        param(0, fb, 0.0),
        param(0, out_level, 1.0),
        param(9000, out_level, 0.0),
    ] {
        events_in.push(e).unwrap();
    }
    let mut events_out = EventList::with_capacity(8);
    let ins: Vec<&[f32]> = input.iter().map(|b| &b[..]).collect();
    let mut out_refs: Vec<&mut [f32]> = outs.iter_mut().map(|b| &mut b[..]).collect();
    p.process(&mut ProcessContext {
        frames: frames as u32,
        transport: &TransportState::default(),
        audio_in: &ins,
        audio_out: &mut out_refs,
        events_in: &events_in,
        events_out: &mut events_out,
    });
    assert!(
        outs[0][..9000].iter().all(|&x| x.abs() > 0.1),
        "muted early or unwritten"
    );
    assert!(
        outs[0][9000..].iter().all(|&x| x == 0.0),
        "not muted at frame 9000"
    );
}

/// More changes in one segment than the pending buffer holds: the last value
/// for a parameter still wins.
#[test]
fn many_changes_in_one_segment_keep_their_order() {
    let dir = need_plugins!();
    // An instrument, so the note goes into the plugin and holds the changes.
    let mut p = load(&dir, "mda.lv2", &format!("{MDA}EPiano"));
    let fb = p.params()[0].id;
    // The note keeps the changes pending (they move to frame 51), so more of
    // them arrive than the pending buffer holds.
    let mut events = vec![note_on(50, 60)];
    events.extend((0..100).map(|i| param(50, fb, f64::from(i) / 200.0)));
    events.push(param(50, fb, 0.9));
    run(&mut *p, &[], &events, &TransportState::default());
    assert!(
        (p.param_value(fb) - 0.9).abs() < 1e-6,
        "got {}",
        p.param_value(fb)
    );
}

/// A copy of mda Delay's metadata whose Feedback port has its bounds the wrong
/// way round, using the installed binary.
fn inverted_range_bundle(dir: &Path) -> tempfile::TempDir {
    let bundle = tempfile::tempdir().unwrap();
    let binary = dir.join("mda.lv2/Delay.so");
    let ttl = format!(
        r#"@prefix doap: <http://usefulinc.com/ns/doap#> .
@prefix lv2: <http://lv2plug.in/ns/lv2core#> .
<{MDA}Delay>
	a lv2:Plugin ;
	doap:name "MDA Delay" ;
	lv2:binary <file://{binary}> ;
	lv2:port [
		a lv2:InputPort , lv2:ControlPort ;
		lv2:index 0 ; lv2:symbol "l_delay" ; lv2:name "L Delay" ;
		lv2:default 0.5 ; lv2:minimum 0.0 ; lv2:maximum 1.0
	] , [
		a lv2:InputPort , lv2:ControlPort ;
		lv2:index 1 ; lv2:symbol "r_delay" ; lv2:name "R Delay" ;
		lv2:default 0.27 ; lv2:minimum 0.0 ; lv2:maximum 1.0
	] , [
		a lv2:InputPort , lv2:ControlPort ;
		lv2:index 2 ; lv2:symbol "feedback" ; lv2:name "Feedback" ;
		lv2:default 0.7 ; lv2:minimum 1.0 ; lv2:maximum 0.0
	] , [
		a lv2:InputPort , lv2:ControlPort ;
		lv2:index 3 ; lv2:symbol "fb_tone" ; lv2:name "Fb Tone" ;
		lv2:default 0.5 ; lv2:minimum 0.0 ; lv2:maximum 1.0
	] , [
		a lv2:InputPort , lv2:ControlPort ;
		lv2:index 4 ; lv2:symbol "fx_mix" ; lv2:name "FX Mix" ;
		lv2:default 0.33 ; lv2:minimum 0.0 ; lv2:maximum 1.0
	] , [
		a lv2:InputPort , lv2:ControlPort ;
		lv2:index 5 ; lv2:symbol "output" ; lv2:name "Output" ;
		lv2:default 0.5 ; lv2:minimum 0.0 ; lv2:maximum 1.0
	] , [
		a lv2:InputPort , lv2:AudioPort ; lv2:index 6 ; lv2:symbol "left_in" ; lv2:name "Left In"
	] , [
		a lv2:InputPort , lv2:AudioPort ; lv2:index 7 ; lv2:symbol "right_in" ; lv2:name "Right In"
	] , [
		a lv2:OutputPort , lv2:AudioPort ; lv2:index 8 ; lv2:symbol "left_out" ; lv2:name "Left Out"
	] , [
		a lv2:OutputPort , lv2:AudioPort ; lv2:index 9 ; lv2:symbol "right_out" ; lv2:name "Right Out"
	] .
"#,
        binary = binary.display()
    );
    std::fs::write(bundle.path().join("manifest.ttl"), ttl).unwrap();
    bundle
}

/// livi clamps every control write to the port's bounds, which panics when
/// they are inverted, so such a plugin is refused up front.
#[test]
fn a_port_with_an_inverted_range_is_refused() {
    let dir = need_plugins!();
    let bundle = inverted_range_bundle(&dir);
    let format = daw_plugins_lv2::format();
    let d = find(&*format, bundle.path(), ".", &format!("{MDA}Delay"));
    let Err(e) = format.instantiate(&d, SR, BLOCK) else {
        panic!("instantiated a plugin whose control writes would panic");
    };
    assert!(e.to_string().contains("Feedback"), "{e}");
}
