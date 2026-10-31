//! Launches the real app offscreen and lets `SelfTest.qml` drive it.
//!
//! Needs Qt 6, Kirigami and qqc2-desktop-style installed, which is why the
//! CI runs this crate on Arch Linux.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::Duration;

/// A plugin folder holding only the CLAP test plugin, built in its own
/// target directory under the system temp folder: CI caches restore
/// `target/` with fingerprints whose libraries were dropped, and cleaning
/// there would race the other test processes copying the library.
fn plugin_folder() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let target = std::env::temp_dir().join("kadence-clap-test-plugin");
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let status = Command::new(cargo)
            .args([
                "build",
                "--quiet",
                "--package",
                "daw-clap-test-plugin",
                "--lib",
            ])
            .arg("--target-dir")
            .arg(&target)
            .current_dir(&root)
            .status()
            .expect("run cargo");
        assert!(status.success(), "building the test plugin failed");
        let dir = tempdir("plugins");
        std::fs::copy(
            target.join("debug").join("libdaw_clap_test_plugin.so"),
            dir.join("test-gain.clap"),
        )
        .expect("copy the test plugin");
        dir
    })
}

fn kadence(config_home: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_kadence"));
    // Scan only the test plugin, with a fresh cache: CLAP also looks in
    // ~/.clap and /usr/lib/clap, which a test machine leaves empty.
    let empty = config_home.join("no-plugins");
    std::fs::create_dir_all(&empty).expect("create an empty plugin folder");
    cmd.env("CLAP_PATH", plugin_folder())
        .env("LV2_PATH", &empty)
        .env("VST3_PATH", &empty)
        .env("XDG_CACHE_HOME", config_home.join("cache"));
    cmd.env("QT_QPA_PLATFORM", "offscreen")
        .env("QT_FORCE_STDERR_LOGGING", "1")
        .env("XDG_CONFIG_HOME", config_home)
        .env("KADENCE_AUDIO", "stub")
        .env_remove("KADENCE_SMOKE_TEST_CONFIG")
        .env_remove("QT_SCALE_FACTOR")
        // Some checks read English messages; keep translations out of it.
        .env("LANGUAGE", "en_US")
        .env("LANG", "C.UTF-8")
        .env_remove("LC_ALL")
        .env_remove("LC_MESSAGES");
    cmd
}

fn run(mut cmd: Command) -> std::process::Output {
    use std::io::Read;

    let mut child = cmd
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("start kadence");
    // Drain both pipes while waiting, or a chatty run blocks on a full pipe.
    let drain = |mut pipe: Box<dyn Read + Send>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            pipe.read_to_end(&mut buf).ok();
            buf
        })
    };
    let stdout = drain(Box::new(child.stdout.take().expect("stdout")));
    let stderr = drain(Box::new(child.stderr.take().expect("stderr")));
    // The self test takes a few seconds; anything near a minute is a hang.
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    let status = loop {
        if let Some(status) = child.try_wait().expect("wait for kadence") {
            break status;
        }
        if std::time::Instant::now() > deadline {
            child.kill().ok();
            child.wait().ok();
            let log = stderr.join().unwrap_or_default();
            panic!(
                "kadence did not exit within 60 s:\n{}",
                String::from_utf8_lossy(&log)
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    std::process::Output {
        status,
        stdout: stdout.join().unwrap_or_default(),
        stderr: stderr.join().unwrap_or_default(),
    }
}

#[test]
fn smoke_test_passes_and_saves_settings() {
    let config = tempdir("smoke");
    let mut cmd = kadence(&config);
    // `--smoke-test` only keeps its config where asked to (see kadence.cpp).
    cmd.arg("--smoke-test")
        .env("KADENCE_SMOKE_TEST_CONFIG", &config);
    let output = run(cmd);
    let log = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "smoke test failed:\n{log}");
    assert!(log.contains("smoke test: passed"), "no pass line:\n{log}");
    assert!(
        log.contains("smoke test: checking the test plugin"),
        "the plugin browser checks did not run:\n{log}"
    );
    assert!(
        log.contains("startup audio: 0 48000 256"),
        "fresh config should start on defaults:\n{log}"
    );
    assert!(
        log.contains("startup message: none"),
        "fresh config should start without an error:\n{log}"
    );
    // Project messages are worded in QML, not Rust (issue #81).
    assert!(
        log.contains("import message: Could not add “take.wav”. It could not be read as audio"),
        "import failure not worded:\n{log}"
    );
    assert!(
        log.contains(
            "open message: Could not open the project. Could not access “/nonexistent/song.kadence"
        ),
        "open failure not worded:\n{log}"
    );

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

    // A second launch starts with the saved settings: device 1, 44.1 kHz,
    // 512 samples.
    let mut cmd = kadence(&config);
    cmd.arg("--smoke-test")
        .env("KADENCE_SMOKE_TEST_CONFIG", &config);
    let output = run(cmd);
    let log = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "second run failed:\n{log}");
    assert!(
        log.contains("startup audio: 1 44100 512"),
        "saved settings not loaded at startup:\n{log}"
    );
}

#[test]
fn unplugged_saved_device_is_explained_at_startup() {
    // The saved device is gone: the app keeps the saved rate and buffer on
    // the default device and says why, in its own words (issue #54).
    let config = tempdir("unplugged");
    std::fs::write(
        config.join("kadencerc"),
        "[Audio]\nBufferSize=128\nDevice=stub:gone\nSampleRate=96000\n",
    )
    .expect("write kadencerc");
    let mut cmd = kadence(&config);
    cmd.arg("--smoke-test")
        .env("KADENCE_SMOKE_TEST_CONFIG", &config);
    let output = run(cmd);
    let log = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "smoke test failed:\n{log}");
    assert!(
        log.contains("startup audio: 0 96000 128"),
        "should fall back to the default device only:\n{log}"
    );
    assert!(
        log.contains(
            "startup message: Your saved audio settings could not be used, so Kadence \
             started with other ones. The audio device “stub:gone” is not available."
        ),
        "startup fallback not explained:\n{log}"
    );
}

#[test]
fn smoke_test_leaves_the_users_config_alone() {
    // A user checking an install runs plain `kadence --smoke-test`; the
    // audio settings it tries out must not replace the ones they saved.
    let config = tempdir("user-config");
    let saved = "[Audio]\nBufferSize=64\nDevice=stub:default\nSampleRate=96000\n";
    std::fs::write(config.join("kadencerc"), saved).expect("write kadencerc");
    let mut cmd = kadence(&config);
    cmd.arg("--smoke-test");
    let output = run(cmd);
    let log = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "smoke test failed:\n{log}");
    assert!(
        log.contains("startup audio: 0 48000 256"),
        "smoke test should start from a fresh config:\n{log}"
    );
    let rc = std::fs::read_to_string(config.join("kadencerc")).expect("read kadencerc");
    assert_eq!(rc, saved, "smoke test changed the user's kadencerc");
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
