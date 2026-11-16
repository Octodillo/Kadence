//! Builds this crate's example plugins and wraps them in `.vst3` bundles. Shared by the
//! integration tests and, through `#[path]` in `src/lib.rs`, the crate's unit tests.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Builds the example `example` and wraps its shared object in `<bundle>.vst3`, in a
/// directory of this process's own named after `tag`. Returns the bundle and the shared
/// object inside it.
///
/// The example is built here, always: `cargo test` does not build examples for every
/// kind of test, and an old copy would test an old plugin. It gets its own target
/// directory, because where cargo keeps a test binary, and so what lies next to it, has
/// changed between cargo versions.
pub fn example_bundle(example: &str, bundle: &str, tag: &str) -> (PathBuf, PathBuf) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let target = root.join("target").join("vst3-test-plugins");
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let status = Command::new(cargo)
        .args(["build", "--quiet", "--package", "daw-plugins-vst3"])
        .args(["--example", example, "--target-dir"])
        .arg(&target)
        .current_dir(&root)
        .status()
        .expect("run cargo to build the test plugin");
    assert!(status.success(), "building the {example} example failed");
    let so = target.join(format!("debug/examples/lib{example}.so"));
    let dir = std::env::temp_dir().join(format!("daw-vst3-{tag}-{}", std::process::id()));
    let bin = dir
        .join(format!("{bundle}.vst3/Contents"))
        .join(format!("{}-linux", std::env::consts::ARCH));
    std::fs::create_dir_all(&bin).unwrap();
    let inner = bin.join(format!("{bundle}.so"));
    std::fs::copy(&so, &inner).expect("copy the test plugin");
    (dir.join(format!("{bundle}.vst3")), inner)
}
