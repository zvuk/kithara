#![cfg(unix)]

use std::{
    env, fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

use anyhow::{Context, Result};
use tempfile::TempDir;

const DEMO_PACKAGE: &str = "com.kithara.lane.demo";
const SERIAL: &str = "phone-1";
const REFUSAL_STDOUT: &str = "Error: no such package";
const REFUSAL_STDERR: &str = "am: denied";

/// A repository with an Android SDK, a device that refuses to stop the demo,
/// and Cargo and Gradle, all recording every call into one trace in the order
/// the lane makes them. The same Cargo is on `PATH` and in `$CARGO`, and
/// everything except the demo stop succeeds.
struct Lane {
    _temp: TempDir,
    root: PathBuf,
    home: PathBuf,
    sdk: PathBuf,
    cargo: PathBuf,
    path: std::ffi::OsString,
    trace: PathBuf,
}

impl Lane {
    fn refusing_demo_stop() -> Result<Self> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("repo");
        let home = temp.path().join("home");
        let sdk = temp.path().join("sdk");
        let bin = temp.path().join("bin");
        let trace = temp.path().join("trace");
        for dir in [
            root.join(".config"),
            root.join("android"),
            home.clone(),
            sdk.join("platform-tools"),
            bin.clone(),
        ] {
            fs::create_dir_all(dir)?;
        }
        fs::write(root.join("Cargo.toml"), "[workspace]\n")?;
        fs::write(
            root.join(".config/xtask.toml"),
            format!(
                r#"
[ext.android]
test_lane = "android"
ffi_crate = "kithara-ffi"
aars = []
default_avd = "Lane_AVD"
demo_package = "{DEMO_PACKAGE}"
demo_activity = "{DEMO_PACKAGE}.MainActivity"
api_level = "26"
"#
            ),
        )?;
        let record = |tool: &str| format!("printf '{tool} %s\\n' \"$*\" >> '{}'", trace.display());
        write_executable(
            &sdk.join("platform-tools/adb"),
            &format!(
                "#!/bin/sh\n{}\ncase \"$*\" in\n  devices*) printf 'List of devices attached\\n{SERIAL}\\tdevice\\n';;\n  *force-stop*) echo '{REFUSAL_STDOUT}'; echo '{REFUSAL_STDERR}' >&2; exit 1;;\nesac\n",
                record("adb")
            ),
        )?;
        write_executable(
            &root.join("android/gradlew"),
            &format!("#!/bin/sh\n{}\n", record("gradlew")),
        )?;
        // Workspace discovery needs a real answer from `cargo metadata`.
        let real_cargo = env::var("CARGO").context("the test runs under Cargo")?;
        let cargo = bin.join("cargo");
        write_executable(
            &cargo,
            &format!(
                "#!/bin/sh\ncase \"$1\" in\n  metadata) exec '{real_cargo}' \"$@\";;\nesac\n{}\n",
                record("cargo")
            ),
        )?;
        let mut paths = vec![bin];
        if let Some(system_path) = env::var_os("PATH") {
            paths.extend(env::split_paths(&system_path));
        }
        let path = env::join_paths(paths).context("construct fixture PATH")?;
        Ok(Self {
            _temp: temp,
            root,
            home,
            sdk,
            cargo,
            path,
            trace,
        })
    }

    fn run_tests(&self) -> Result<Output> {
        Command::new(env!("CARGO_BIN_EXE_xtask"))
            .args(["android", "test", "--skip-build", "--serial", SERIAL])
            .current_dir(&self.root)
            .env("CARGO", &self.cargo)
            .env("HOME", &self.home)
            .env("ANDROID_HOME", &self.sdk)
            .env("PATH", &self.path)
            .env_remove("ANDROID_SDK_ROOT")
            .env_remove("CARGO_TARGET_DIR")
            .env_remove("KITHARA_ANDROID_TEST_FILTER")
            .stdin(Stdio::null())
            .output()
            .context("run the Android lane")
    }

    fn calls(&self) -> Vec<String> {
        fs::read_to_string(&self.trace)
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

fn write_executable(path: &Path, body: &str) -> Result<()> {
    fs::write(path, body)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
    Ok(())
}

#[test]
fn a_refused_demo_stop_ends_the_lane_before_any_test_is_prepared() -> Result<()> {
    let lane = Lane::refusing_demo_stop()?;

    let output = lane.run_tests()?;
    let calls = lane.calls();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "the lane passed although the demo could not be stopped: {calls:#?}"
    );
    for part in [DEMO_PACKAGE, REFUSAL_STDOUT, REFUSAL_STDERR] {
        assert!(
            stderr.contains(part),
            "the lane failure does not report `{part}` from the refused stop: {stderr}"
        );
    }
    assert!(
        calls.iter().any(|call| call.starts_with("adb ")
            && call.contains("force-stop")
            && call.ends_with(DEMO_PACKAGE)),
        "the lane never asked the device to stop {DEMO_PACKAGE}: {calls:#?}\n{stderr}"
    );
    let preparing: Vec<&String> = calls
        .iter()
        .filter(|call| {
            call.starts_with("cargo ") || call.starts_with("gradlew") || call.contains(" reverse ")
        })
        .collect();
    assert!(
        preparing.is_empty(),
        "the lane went on to prepare tests with the demo still running: {preparing:#?}"
    );
    Ok(())
}
