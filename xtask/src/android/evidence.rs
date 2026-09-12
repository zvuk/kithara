//! Per-run Android test artifacts and preparation/cleanup results.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::device::{self, Selected};
use crate::{BuildProfile, child};

pub(super) struct Dir {
    root: PathBuf,
}

impl Dir {
    pub(super) fn path(&self) -> &Path {
        &self.root
    }
    /// Allocate a new directory without replacing any previous run.
    pub(super) fn create(workspace_root: &Path) -> Result<Self> {
        let root = workspace_root.join("target/android-test").join(format!(
            "run-{}-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .context("clock precedes Unix epoch")?
                .as_nanos(),
            std::process::id()
        ));
        fs::create_dir_all(workspace_root.join("target/android-test"))?;
        fs::create_dir(&root).with_context(|| format!("creating {}", root.display()))?;
        println!("==> Run evidence: {}", root.display());
        Ok(Self { root })
    }

    pub(super) fn gradle_log(&self) -> PathBuf {
        self.root.join("gradle.log")
    }
    pub(super) fn results(&self) -> PathBuf {
        self.root.join("test-results")
    }
    pub(super) fn report(&self) -> PathBuf {
        self.root.join("test-report")
    }

    pub(super) fn server_log(&self) -> PathBuf {
        self.root.join("server.log")
    }

    pub(super) fn manifest(&self) -> PathBuf {
        self.root.join("manifest.json")
    }

    /// A failure here costs a diagnostic, so it is reported and left at that.
    pub(super) fn capture_logcat(&self, device: &Selected) {
        let path = self.root.join("logcat.log");
        let captured = device::control(device.adb().args(["logcat", "-d", "-v", "time"]), None)
            .context("failed to run `adb logcat -d`")
            .and_then(|output| {
                fs::write(&path, output.stdout)
                    .with_context(|| format!("writing {}", path.display()))
            });
        if let Err(error) = captured {
            println!("==> logcat was not captured: {error:#}");
        }
    }
}

/// On disk from the moment the run starts preparing, and updated at every step.
pub(super) struct Manifest {
    path: PathBuf,
    value: Value,
}

impl Manifest {
    /// Instrumentation runner argument carrying the fixture server's address.
    pub(super) const URL_ARGUMENT: &'static str = "KITHARA_TEST_SERVER_URL";

    /// Written before the run takes anything, so a run that fails while
    /// preparing still leaves a record behind.
    pub(super) fn open(dir: &Dir, workspace_root: &Path, profile: BuildProfile) -> Result<Self> {
        let manifest = Self {
            path: dir.manifest(),
            value: json!({
                "commit": git(workspace_root, &["rev-parse", "HEAD"]),
                "dirty": git(workspace_root, &["status", "--porcelain"])
                    .is_some_and(|status| !status.is_empty()),
                "started_unix": now(),
                "profile": profile.to_string(),
                "stages": {},
                "test_results": dir.results(),
                "test_report": dir.report(),
                "gradle_log": dir.gradle_log(),
            }),
        };
        manifest.write()?;
        Ok(manifest)
    }

    /// Keep what a step answered and hand it back, so the reason a run stopped
    /// is on disk.
    pub(super) fn stage<T>(&mut self, name: &str, outcome: Result<T>) -> Result<T> {
        self.value["stages"][name] = match &outcome {
            Ok(_) => json!("ok"),
            Err(error) => json!(format!("{error:#}")),
        };
        self.flush();
        outcome
    }

    pub(super) fn device(
        &mut self,
        workspace_root: &Path,
        device: &Selected,
        cancel: &child::Cancel,
    ) {
        self.value["jni"] = jni_libraries(workspace_root);
        self.value["device"] = json!({
            "serial": device.serial,
            "api_level": getprop(device, "ro.build.version.sdk", cancel),
            "abis": getprop(device, "ro.product.cpu.abilist", cancel),
            "avd": avd_name(device, cancel),
            "ownership": if device.owns_emulator() { "started by this run" } else { "borrowed" },
        });
        self.flush();
    }

    pub(super) fn fixture_server(&mut self, host_url: &str, device_url: &str, device_port: u16) {
        self.value["fixture_server"] = json!({
            "host_url": host_url,
            "device_url": device_url,
            "device_port": device_port,
            "runner_argument": Self::URL_ARGUMENT,
        });
        self.flush();
    }

    pub(super) fn instrumentation(&mut self, cases: &[String]) {
        self.value["instrumentation"] = json!({ "passed": cases.len(), "cases": cases });
        self.flush();
    }

    pub(super) fn gradle_exit(&mut self, code: Option<i32>) {
        self.value["gradle_exit_code"] = json!(code);
        self.flush();
    }

    /// Record cleanup even when preparation acquired no device.
    pub(super) fn cleanup(
        &mut self,
        unmapped: &Result<()>,
        stopped: &Result<()>,
        released: &Result<()>,
        owned_emulator: Option<bool>,
        owned_reverse: bool,
    ) {
        self.value["cleanup"] = json!({
            "reverse_mapping": if owned_reverse { outcome(unmapped) } else { json!("not acquired; inspect acquisition stage if present") },
            "fixture_server": outcome(stopped),
            "emulator": match owned_emulator { Some(true) => outcome(released), Some(false) => json!("borrowed"), None => json!("not acquired") },
        });
        self.value["finished_unix"] = json!(now());
        self.flush();
    }

    pub(super) fn write(&self) -> Result<()> {
        let body =
            serde_json::to_string_pretty(&self.value).context("serializing the run manifest")?;
        fs::write(&self.path, body).with_context(|| format!("writing {}", self.path.display()))
    }

    /// A record that cannot be written costs a diagnostic; the run itself is
    /// unaffected until its last write.
    fn flush(&mut self) {
        if let Err(error) = self.write() {
            println!("==> Run manifest was not updated: {error:#}");
        }
    }
}

fn outcome(result: &Result<()>) -> Value {
    match result {
        Ok(()) => json!("ok"),
        Err(error) => json!(format!("{error:#}")),
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

fn git(root: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// The emulator publishes its AVD under one of two properties depending on the
/// image, and a physical device under neither.
fn avd_name(device: &Selected, cancel: &child::Cancel) -> Option<String> {
    getprop(device, "ro.boot.qemu.avd_name", cancel)
        .or_else(|| getprop(device, "ro.kernel.qemu.avd_name", cancel))
}

fn getprop(device: &Selected, name: &str, cancel: &child::Cancel) -> Option<String> {
    let output =
        device::control(device.adb().args(["shell", "getprop", name]), Some(cancel)).ok()?;
    let value = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (output.status.success() && !value.is_empty()).then_some(value)
}

/// Hash the JNI libraries used by the run.
fn jni_libraries(workspace_root: &Path) -> Value {
    let root = workspace_root.join("android/lib/build/generated/jniLibs");
    let Ok(abis) = fs::read_dir(&root) else {
        return Value::Null;
    };
    let mut libraries = serde_json::Map::new();
    for abi in abis.filter_map(std::result::Result::ok) {
        let library = abi.path().join("libkithara_ffi.so");
        if let Ok(bytes) = fs::read(&library) {
            libraries.insert(
                abi.file_name().to_string_lossy().into_owned(),
                json!(hex::encode(Sha256::digest(&bytes))),
            );
        }
    }
    Value::Object(libraries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_run_writes_below_the_shared_evidence_root_rather_than_into_it() {
        let workspace = tempfile::tempdir().expect("temporary directory");
        let dir = Dir::create(workspace.path()).unwrap();
        let shared = workspace.path().join("target/android-test");

        assert_ne!(dir.root, shared, "runs would share one manifest");
        assert_eq!(dir.root.parent(), Some(shared.as_path()));
        assert!(
            dir.root.file_name().is_some_and(|name| name
                .to_string_lossy()
                .contains(&std::process::id().to_string())),
            "{} does not name the run that owns it",
            dir.root.display()
        );
    }

    #[test]
    fn a_manifest_exists_before_the_run_takes_anything() {
        let workspace = tempfile::tempdir().expect("temporary directory");
        let dir = Dir::create(workspace.path()).unwrap();
        let record = Manifest::open(&dir, workspace.path(), BuildProfile::Debug).unwrap();
        let body = fs::read_to_string(record.path).expect("the manifest is on disk");
        assert!(body.contains("started_unix"), "{body}");
        assert!(!body.contains("finished_unix"), "{body}");
    }

    #[test]
    fn failed_reverse_acquisition_is_not_reported_as_successful_cleanup() {
        let workspace = tempfile::tempdir().unwrap();
        let dir = Dir::create(workspace.path()).unwrap();
        let mut record = Manifest::open(&dir, workspace.path(), BuildProfile::Debug).unwrap();
        record
            .stage::<()>(
                "reverse_mapping",
                Err(anyhow::anyhow!(
                    "reverse acquisition outcome unknown; mapping may remain"
                )),
            )
            .unwrap_err();
        record.cleanup(&Ok(()), &Ok(()), &Ok(()), Some(false), false);
        let value: Value = serde_json::from_slice(&fs::read(dir.manifest()).unwrap()).unwrap();
        assert_eq!(value["cleanup"]["emulator"], "borrowed");
        assert_eq!(
            value["cleanup"]["reverse_mapping"],
            "not acquired; inspect acquisition stage if present"
        );
        assert!(value["finished_unix"].is_number());
        assert_eq!(
            value["stages"]["reverse_mapping"],
            "reverse acquisition outcome unknown; mapping may remain"
        );
    }

    #[test]
    fn a_failed_stage_reaches_the_manifest_without_a_later_write() {
        let workspace = tempfile::tempdir().expect("temporary directory");
        let dir = Dir::create(workspace.path()).unwrap();
        let mut record = Manifest::open(&dir, workspace.path(), BuildProfile::Debug).unwrap();

        let outcome: Result<()> = record.stage("device", Err(anyhow::anyhow!("no device online")));

        assert!(outcome.is_err());
        let body = fs::read_to_string(dir.manifest()).expect("the manifest is on disk");
        assert!(body.contains("no device online"), "{body}");
    }
}
