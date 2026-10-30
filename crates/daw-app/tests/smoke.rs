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
        .env("KADENCE_AUDIO", "stub")
        .env_remove("QT_SCALE_FACTOR");
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
    cmd.arg("--smoke-test");
    let output = run(cmd);
    let log = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "smoke test failed:\n{log}");
    assert!(log.contains("smoke test: passed"), "no pass line:\n{log}");
    assert!(
        log.contains("startup audio: 0 48000 256"),
        "fresh config should start on defaults:\n{log}"
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
    cmd.arg("--smoke-test");
    let output = run(cmd);
    let log = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "second run failed:\n{log}");
    assert!(
        log.contains("startup audio: 1 44100 512"),
        "saved settings not loaded at startup:\n{log}"
    );
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
