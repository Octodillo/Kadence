//! Launches the real app offscreen and lets `SelfTest.qml` drive it.
//!
//! Needs Qt 6, Kirigami and qqc2-desktop-style installed, which is why the
//! CI runs this crate on Arch Linux.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

fn kadence(config_home: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_kadence"));
    cmd.env("QT_QPA_PLATFORM", "offscreen")
        .env("QT_FORCE_STDERR_LOGGING", "1")
        .env("XDG_CONFIG_HOME", config_home)
        .env_remove("QT_SCALE_FACTOR");
    cmd
}

fn run(mut cmd: Command) -> std::process::Output {
    let mut child = cmd
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("start kadence");
    // The self test takes a few seconds; anything near a minute is a hang.
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while child.try_wait().expect("wait for kadence").is_none() {
        if std::time::Instant::now() > deadline {
            child.kill().ok();
            panic!("kadence did not exit within 60 s");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    child.wait_with_output().expect("collect kadence output")
}

#[test]
fn smoke_test_passes_and_saves_settings() {
    let config = tempdir("smoke");
    let mut cmd = kadence(&config);
    cmd.arg("--smoke-test");
    let output = run(cmd);
    let log = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "smoke test failed:\n{log}");
    assert!(log.contains("smoke test: passed"), "no pass line:\n{log}");

    // SelfTest.qml leaves these audio settings behind, through KConfig.
    let rc = std::fs::read_to_string(config.join("kadencerc")).expect("read kadencerc");
    for line in [
        "[Audio]",
        "Device=stub:default",
        "SampleRate=44100",
        "BufferSize=512",
    ] {
        assert!(rc.contains(line), "kadencerc lacks {line:?}:\n{rc}");
    }

    // A second launch picks the saved settings up again.
    let mut cmd = kadence(&config);
    cmd.arg("--smoke-test");
    assert!(run(cmd).status.success());
}

#[test]
fn version_flag_comes_from_kaboutdata() {
    let config = tempdir("version");
    let mut cmd = kadence(&config);
    cmd.arg("--version");
    let output = run(cmd);
    assert!(output.status.success());
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.contains(concat!("kadence ", env!("CARGO_PKG_VERSION"))),
        "unexpected --version output: {text}"
    );
}

fn tempdir(name: &str) -> std::path::PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}
