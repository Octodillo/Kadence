//! Module lifecycle: loading the same module from many threads, and modules that fail to
//! load. A separate test binary from `vst3.rs`, so no other test holds the modules.

use std::path::{Path, PathBuf};
use std::process::Command;

use daw_core::PluginFormat;
use daw_plugins_vst3::Vst3Format;
use libloading::os::unix::Library;

/// Wraps the example `name`'s shared object in `<bundle>.vst3` and returns the bundle and
/// the shared object inside it.
fn bundle(name: &str, bundle: &str) -> (PathBuf, PathBuf) {
    // target/<profile>/deps/<test binary>
    let profile_dir = std::env::current_exe()
        .unwrap()
        .parent()
        .and_then(Path::parent)
        .unwrap()
        .to_path_buf();
    let so = profile_dir.join(format!("examples/lib{name}.so"));
    // `cargo test --test module` does not rebuild examples, so build it here, always (an old
    // copy may lack the counters), and in this test binary's profile.
    let profile = match profile_dir.file_name().and_then(|n| n.to_str()) {
        Some("debug") | None => "dev",
        Some(name) => name,
    };
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let status = Command::new(cargo)
        .args(["build", "-p", "daw-plugins-vst3", "--example", name])
        .args(["--profile", profile])
        .status()
        .expect("run cargo to build the test plugin");
    assert!(status.success(), "building the {name} example failed");
    let root = std::env::temp_dir().join(format!("daw-vst3-module-{}", std::process::id()));
    let bin = root
        .join(format!("{bundle}.vst3/Contents"))
        .join(format!("{}-linux", std::env::consts::ARCH));
    std::fs::create_dir_all(&bin).unwrap();
    let inner = bin.join(format!("{bundle}.so"));
    std::fs::copy(&so, &inner).unwrap();
    (root.join(format!("{bundle}.vst3")), inner)
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
                    let d = Vst3Format.scan(&bundle).pop().expect("testgain scans");
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
    let d = Vst3Format.scan(&bundle).pop().unwrap();
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
    assert!(Vst3Format.scan(&bundle).is_empty());
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
