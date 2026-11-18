//! Module lifecycle: loading the same module from many threads, and modules that fail to
//! load. A separate test binary from `vst3.rs`, so no other test holds the modules.

use std::path::{Path, PathBuf};

use daw_core::PluginFormat;
use daw_core::plugin::PluginError;
use daw_plugins_vst3::Vst3Format;
use libloading::os::unix::Library;

mod support;

fn bundle(example: &str, bundle: &str) -> (PathBuf, PathBuf) {
    support::example_bundle(example, bundle, "module")
}

/// Opens the module's shared object ourselves, so its counters stay readable (and keep
/// their values) whatever the host does with the module.
fn counters(so: &Path) -> Library {
    unsafe { Library::open(Some(so), libc::RTLD_NOW | libc::RTLD_LOCAL) }.unwrap()
}

#[test]
fn a_module_is_entered_once_however_many_threads_load_it() {
    let (bundle, so) = bundle("testgain", "TestGain");
    let lib = counters(&so);
    let counts = || {
        let mut out = [0u32; 3];
        unsafe {
            let f = lib
                .get::<unsafe extern "C" fn(*mut u32)>(b"testgain_module_counts\0")
                .unwrap();
            f(out.as_mut_ptr());
        }
        out
    };

    // Each thread keeps creating and dropping the only instance it has, so with modules
    // unloaded on the last drop, loads and unloads of the same module would interleave.
    std::thread::scope(|s| {
        for _ in 0..8 {
            s.spawn(|| {
                for _ in 0..10 {
                    let d = Vst3Format
                        .scan(&bundle)
                        .unwrap()
                        .plugins
                        .pop()
                        .expect("testgain scans");
                    let p = Vst3Format.instantiate(&d, 48_000.0, 64).unwrap();
                    assert_eq!(p.descriptor().name, "Test Gain");
                }
            });
        }
    });

    let [entries, exits, overlaps] = counts();
    assert_eq!(overlaps, 0, "ModuleEntry/ModuleExit ran concurrently");
    // Loaded once and kept: plugins leave run loop handlers behind, so their code must stay
    // mapped.
    assert_eq!((entries, exits), (1, 0));

    // Shutdown exits only modules no instance uses, and a later load enters them again.
    let d = Vst3Format.scan(&bundle).unwrap().plugins.pop().unwrap();
    let p = Vst3Format.instantiate(&d, 48_000.0, 64).unwrap();
    daw_plugins_vst3::shutdown();
    assert_eq!(counts(), [1, 0, 0], "exited a module in use");
    drop(p);
    // Through the format, as the sandbox host and the scanner do it.
    Vst3Format.shutdown();
    assert_eq!(counts(), [1, 1, 0]);
    let p = Vst3Format.instantiate(&d, 48_000.0, 64).unwrap();
    drop(p);
    daw_plugins_vst3::shutdown();
    assert_eq!(counts(), [2, 2, 0]);
}

#[test]
fn a_module_without_a_factory_is_left_as_it_was() {
    let (bundle, so) = bundle("nofactory", "NoFactory");
    let lib = counters(&so);
    // The scan says why there are no plugins.
    match Vst3Format.scan(&bundle) {
        Err(PluginError::Load(why)) => assert!(why.contains("GetPluginFactory"), "{why}"),
        other => panic!("expected a load error, got {other:?}"),
    }
    let mut out = [0u32; 2];
    unsafe {
        let f = lib
            .get::<unsafe extern "C" fn(*mut u32)>(b"nofactory_module_counts\0")
            .unwrap();
        f(out.as_mut_ptr());
    }
    let [entries, exits] = out;
    assert_eq!(
        entries, exits,
        "ModuleEntry ran without a matching ModuleExit"
    );
}
