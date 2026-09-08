use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::PathBuf,
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(0);
const SAMPLE: &str = include_str!("../../../config/plan-beispiel.toml");

struct ConfigFile(PathBuf);
impl ConfigFile {
    fn new(text: &str) -> Self {
        let path = PathBuf::from(format!(
            "/tmp/uplink-config-test-{}-{}.toml",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        file.write_all(text.as_bytes()).unwrap();
        Self(path)
    }
    fn run(&self) -> Output {
        Command::new(env!("CARGO_BIN_EXE_uplink"))
            .arg("plan")
            .arg("--config")
            .arg(&self.0)
            .output()
            .unwrap()
    }
}
impl Drop for ConfigFile {
    fn drop(&mut self) {
        fs::remove_file(&self.0).unwrap();
    }
}

#[test]
fn example_plans_one_encode_with_two_audio_routes() {
    let output = ConfigFile::new(SAMPLE).run();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("Video-Encodes: 1"));
    assert!(stdout.contains("Nur Szenarioplan"));
    assert!(stdout.contains("VOD-Ton: Spur 3"));
}

#[test]
fn unknown_fields_fail_without_repeating_config_values() {
    let output = ConfigFile::new(&format!(
        "unexpected = 'DO_NOT_ECHO_CONFIG_VALUE'\n{SAMPLE}"
    ))
    .run();
    assert_eq!(output.status.code(), Some(2));
    assert!(
        !String::from_utf8(output.stderr)
            .unwrap()
            .contains("DO_NOT_ECHO_CONFIG_VALUE")
    );
}

#[test]
fn rejected_output_is_visible_and_returns_nonzero() {
    let text = SAMPLE.replacen(
        "allowed_video_profiles = [\"h264_1080\"]",
        "allowed_video_profiles = []",
        1,
    );
    let output = ConfigFile::new(&text).run();
    assert_eq!(output.status.code(), Some(3));
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("abgelehnt")
    );
}

#[test]
fn unknown_profile_reference_and_oversized_config_fail() {
    let output = ConfigFile::new(&SAMPLE.replacen(
        "video_profile = \"av1_1080\"",
        "video_profile = \"unknown\"",
        1,
    ))
    .run();
    assert_eq!(output.status.code(), Some(2));
    let output = ConfigFile::new(&"x".repeat(262145)).run();
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn unsupported_command_does_not_succeed_silently() {
    let output = Command::new(env!("CARGO_BIN_EXE_uplink"))
        .arg("serve")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
}
