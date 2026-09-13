mod build;
mod link;
mod report;
mod runner;

use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    fs,
    io::Read as _,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{Context, Result, bail};
use kithara_devtools::{common::project::ProjectConfig, lock::FileLock, test::NextestAction};
use sha2::{Digest, Sha256};

use super::device::Selected;
use crate::child;

fn art_nextest_list_extra(target: &str, packages: &BTreeSet<String>) -> Vec<String> {
    [
        vec![
            "--no-default-features".into(),
            "--features".into(),
            super::device_features(crate::BuildProfile::Debug).into(),
            "--tests".into(),
            "--target".into(),
            target.into(),
            "--profile".into(),
            "android".into(),
            "--list-type".into(),
            "binaries-only".into(),
            "--message-format".into(),
            "json".into(),
        ],
        packages
            .iter()
            .flat_map(|package| ["-p".to_owned(), package.clone()])
            .collect(),
    ]
    .concat()
}

fn android_product_packages(root: &Path, target: &str, product: &str) -> Result<BTreeSet<String>> {
    let output = Command::new("cargo")
        .current_dir(root)
        .args([
            "tree",
            "-p",
            product,
            "--target",
            target,
            "--no-default-features",
            "--features",
            super::device_features(crate::BuildProfile::Release),
            "--edges",
            "normal",
            "--prefix",
            "none",
            "--format",
            "{p}",
            "--locked",
        ])
        .output()
        .context("resolving shipped Android package graph")?;
    if !output.status.success() {
        bail!(
            "resolving shipped Android package graph failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let packages = product_packages_from_tree(root, &String::from_utf8(output.stdout)?)?;
    if !packages.contains(product) {
        bail!("Android product package `{product}` is absent from its resolved graph");
    }
    Ok(packages)
}

fn command_packages(command: &Command) -> Result<BTreeSet<String>> {
    let mut packages = BTreeSet::new();
    let mut arguments = command.get_args();
    while let Some(argument) = arguments.next() {
        let Some(argument) = argument.to_str() else {
            continue;
        };
        match argument {
            "-p" | "--package" => {
                let package = arguments
                    .next()
                    .context("Cargo package selector has no value")?
                    .to_str()
                    .context("Cargo package selector is not UTF-8")?;
                packages.insert(package.to_owned());
            }
            _ => {
                if let Some(package) = argument.strip_prefix("--package=") {
                    packages.insert(package.to_owned());
                }
            }
        }
    }
    if packages.is_empty() {
        bail!("Android test lane selects no Cargo packages");
    }
    Ok(packages)
}

fn product_packages_from_tree(root: &Path, tree: &str) -> Result<BTreeSet<String>> {
    let workspace_prefix = format!("({}/", root.display());
    let mut packages = BTreeSet::new();
    for line in tree.lines().filter(|line| line.contains(&workspace_prefix)) {
        let package = line
            .split_ascii_whitespace()
            .next()
            .context("cargo tree emitted an empty package line")?;
        packages.insert(package.to_owned());
    }
    Ok(packages)
}

pub(crate) struct Prepared {
    root: PathBuf,
    evidence: PathBuf,
    target: String,
    cargo_target: PathBuf,
    environment: BTreeMap<String, String>,
    session: runner::Session,
    _cache_lease: FileLock,
}

pub(crate) fn prepare(
    root: &Path,
    config: &ProjectConfig,
    device: &Selected,
    evidence_dir: &Path,
    cancel: &child::Cancel,
) -> Result<Prepared> {
    build::prepare(root, config, device, evidence_dir, cancel)
}

pub(crate) fn link(args: &[OsString]) -> Result<()> {
    link::run(args)
}

pub(crate) fn run_binary(session: &Path, binary: &Path, args: &[String]) -> Result<i32> {
    runner::run(session, binary, args)
}

impl Prepared {
    pub(crate) fn run(&self, device_url: &str, cancel: &child::Cancel) -> Result<()> {
        let result = self.run_inner(device_url, cancel);
        let _ = write_cache_stats(&self.session, &self.evidence);
        let stopped = self.session.stop();
        let removed = self.session.control(
            &[
                "run-as",
                &self.session.package,
                "rm",
                "-rf",
                &self.session.directory,
            ],
            None,
        );
        let removed = removed.map(|_| ());
        let cleanup = stopped.and(removed);
        match (result, cleanup) {
            (Err(error), Err(cleanup)) => Err(error.context(cleanup)),
            (result, Ok(())) => result,
            (Ok(()), Err(error)) => Err(error),
        }
    }

    fn run_inner(&self, device_url: &str, cancel: &child::Cancel) -> Result<()> {
        let started = std::time::Instant::now();
        let mut session = self.session.clone();
        session
            .environment
            .extend(art_session_environment(&session.directory, device_url));
        println!(
            "==> Native fixtures from {device_url} after {:.1}s (no store push)",
            started.elapsed().as_secs_f64()
        );
        fs::write(
            self.evidence.join("session.json"),
            serde_json::to_vec_pretty(&session)?,
        )?;
        let mut list = self.nextest(NextestAction::List)?;
        list.args(["--message-format", "json"]);
        logged(
            &mut list,
            &self.evidence.join("list.json"),
            &self.evidence.join("list.log"),
            cancel,
        )?;
        let junit = inventory_junit(&self.root);
        if junit.exists() {
            fs::remove_file(&junit)?;
        }
        let mut run = self.nextest(NextestAction::Run)?;
        let result = logged(
            &mut run,
            &self.evidence.join("run.stdout.log"),
            &self.evidence.join("run.log"),
            cancel,
        );
        if junit.is_file() {
            fs::copy(&junit, self.evidence.join("junit.xml"))?;
        }
        result?;
        if !self.evidence.join("junit.xml").is_file() {
            bail!("Android nextest run did not produce JUnit");
        }
        Ok(())
    }

    fn nextest(&self, action: NextestAction) -> Result<Command> {
        let executable = std::env::current_exe()?;
        let runner = serde_json::to_string(&[
            executable.display().to_string(),
            "android".into(),
            "native-runner".into(),
            "--session".into(),
            self.evidence.join("session.json").display().to_string(),
        ])?;
        let mut command = inventory_nextest_command(
            action,
            &self.target,
            &self.evidence.join("binaries.json"),
            &self.evidence.join("cargo-metadata.json"),
            &runner,
        );
        command
            .current_dir(&self.root)
            .envs(&self.environment)
            .env_remove("FFMPEG_DIR")
            .env("CARGO_TARGET_DIR", &self.cargo_target);
        Ok(command)
    }
}

/// Replay an already-built nextest inventory. `--binaries-metadata` rejects
/// cargo selection and build flags, so this command cannot go through a test
/// lane prefix.
fn inventory_nextest_command(
    action: NextestAction,
    target: &str,
    binaries_metadata: &Path,
    cargo_metadata: &Path,
    runner: &str,
) -> Command {
    let mut command = Command::new("cargo");
    command.args([
        "nextest",
        match action {
            NextestAction::List => "list",
            NextestAction::Run => "run",
        },
        "--profile",
        "android",
    ]);
    command
        .arg("--binaries-metadata")
        .arg(binaries_metadata)
        .arg("--cargo-metadata")
        .arg(cargo_metadata)
        .arg("--config")
        .arg(format!("target.{target}.runner={runner}"));
    command
}

/// Inventory nextest writes the profile store under the workspace `target`
/// directory, not `CARGO_TARGET_DIR`.
fn inventory_junit(root: &Path) -> PathBuf {
    root.join("target/nextest/android/junit.xml")
}

fn logged(
    command: &mut Command,
    stdout: &Path,
    stderr: &Path,
    cancel: &child::Cancel,
) -> Result<()> {
    command
        .stdout(Stdio::from(fs::File::create(stdout)?))
        .stderr(Stdio::from(fs::File::create(stderr)?));
    let status = child::run(command, Some(cancel))
        .with_context(|| format!("running command; see {}", stderr.display()))?;
    if !status.success() {
        bail!("command failed ({status}); see {}", stderr.display());
    }
    Ok(())
}

fn sha256(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0_u8; 65536];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(hex::encode(hash.finalize()))
}

fn art_session_environment(directory: &str, device_url: &str) -> BTreeMap<String, String> {
    BTreeMap::from([
        (
            "KITHARA_FIXTURE_CACHE".into(),
            format!("{directory}/fixtures"),
        ),
        ("KITHARA_FIXTURE_ORIGIN".into(), device_url.to_owned()),
        ("KITHARA_TEST_SERVER_URL".into(), device_url.to_owned()),
        ("TMPDIR".into(), directory.to_owned()),
    ])
}

fn write_cache_stats(session: &runner::Session, evidence: &Path) -> Result<()> {
    let cache = format!("{}/fixtures", session.directory);
    let output = session.control(
        &["run-as", &session.package, "toybox", "du", "-ak", &cache],
        None,
    )?;
    fs::write(evidence.join("fixture-cache.log"), &output.stdout)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args_of(cmd: &Command) -> Vec<String> {
        cmd.get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn inventory_nextest_uses_only_the_binaries_metadata_surface() {
        let binaries = Path::new("binaries.json");
        let cargo_metadata = Path::new("cargo-metadata.json");
        let runner = r#"["xtask","android","native-runner","--session","session.json"]"#;
        for (action, verb) in [(NextestAction::List, "list"), (NextestAction::Run, "run")] {
            let command = inventory_nextest_command(
                action,
                "aarch64-linux-android",
                binaries,
                cargo_metadata,
                runner,
            );
            assert_eq!(command.get_program().to_string_lossy(), "cargo");
            assert_eq!(
                args_of(&command),
                [
                    "nextest",
                    verb,
                    "--profile",
                    "android",
                    "--binaries-metadata",
                    "binaries.json",
                    "--cargo-metadata",
                    "cargo-metadata.json",
                    "--config",
                    &format!("target.aarch64-linux-android.runner={runner}"),
                ]
            );
        }
    }

    #[test]
    fn inventory_junit_lives_under_the_workspace_target() {
        assert_eq!(
            inventory_junit(Path::new("/workspace")),
            Path::new("/workspace/target/nextest/android/junit.xml")
        );
    }

    #[test]
    fn art_session_reads_store_records_from_the_reverse_origin() {
        let env = art_session_environment(
            "/data/user/0/com.kithara.nativetest/files/run-1",
            "http://127.0.0.1:38833",
        );
        assert_eq!(
            env.get("KITHARA_FIXTURE_CACHE").map(String::as_str),
            Some("/data/user/0/com.kithara.nativetest/files/run-1/fixtures")
        );
        assert_eq!(
            env.get("KITHARA_FIXTURE_ORIGIN").map(String::as_str),
            Some("http://127.0.0.1:38833")
        );
        assert_eq!(
            env.get("KITHARA_TEST_SERVER_URL"),
            env.get("KITHARA_FIXTURE_ORIGIN")
        );
        assert_eq!(
            env.get("TMPDIR").map(String::as_str),
            Some("/data/user/0/com.kithara.nativetest/files/run-1")
        );
    }

    #[test]
    fn product_tree_keeps_only_workspace_packages() {
        let root = Path::new("/workspace");
        let tree = "\
kithara-ffi v0.0.1 (/workspace/crates/kithara-ffi)\n\
bytes v1.0.0\n\
kithara v0.0.1 (/workspace/crates/kithara)\n\
kithara-stream v0.0.1 (/workspace/crates/kithara-stream)\n";
        assert_eq!(
            product_packages_from_tree(root, tree).unwrap(),
            BTreeSet::from([
                "kithara".to_owned(),
                "kithara-ffi".to_owned(),
                "kithara-stream".to_owned(),
            ])
        );
    }

    #[test]
    fn art_nextest_list_extra_selects_positive_packages() {
        let packages = BTreeSet::from(["kithara-audio".to_owned()]);
        let extra = art_nextest_list_extra("aarch64-linux-android", &packages);
        let (flags, tail) = extra.split_at(extra.len() - 2);
        assert_eq!(
            flags,
            [
                "--no-default-features",
                "--features",
                crate::android::device_features(crate::BuildProfile::Debug),
                "--tests",
                "--target",
                "aarch64-linux-android",
                "--profile",
                "android",
                "--list-type",
                "binaries-only",
                "--message-format",
                "json",
            ]
        );
        assert_eq!(tail, ["-p", "kithara-audio"]);
        assert!(!extra.iter().any(|argument| argument == "--exclude"));
    }

    #[test]
    fn configured_android_lane_combines_product_and_detached_suites() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("xtask lives below the workspace root");
        let config = ProjectConfig::load(root).expect("project config");
        let product = android_product_packages(root, "aarch64-linux-android", "kithara-ffi")
            .expect("Android product packages");
        let extra = art_nextest_list_extra("aarch64-linux-android", &product);
        let command = kithara_devtools::test::nextest_command_for_lane(
            &config,
            "android",
            &extra,
            NextestAction::List,
        )
        .expect("Android lane command");
        let names = command_packages(&command).expect("selected packages");

        assert!(names.contains("kithara-stream"));
        assert!(names.contains("kithara-stream-tests"));
        assert!(!names.contains("kithara-app"));
        assert!(!names.contains("kithara-app-tests"));
        assert!(!names.contains("kithara-beat"));
        assert!(!names.contains("kithara-encode-tests"));
    }

    #[test]
    fn package_selection_evidence_reads_all_cargo_spellings() {
        let mut command = Command::new("cargo");
        command.args([
            "nextest",
            "list",
            "-p",
            "kithara-audio",
            "--package",
            "kithara-audio-tests",
            "--package=kithara-stream-tests",
        ]);

        assert_eq!(
            command_packages(&command).unwrap(),
            BTreeSet::from([
                "kithara-audio".to_owned(),
                "kithara-audio-tests".to_owned(),
                "kithara-stream-tests".to_owned(),
            ])
        );
    }

    #[test]
    fn nextest_inventory_drops_images_that_were_not_packaged() {
        let mut inventory = serde_json::json!({
            "rust-build-meta": {"keep": true},
            "rust-binaries": {
                "kithara-assets": {
                    "binary-id": "kithara-assets",
                    "kind": "lib",
                    "binary-path": "/tmp/kithara_assets-lib"
                },
                "kithara-assets::crash_recovery": {
                    "binary-id": "kithara-assets::crash_recovery",
                    "kind": "test",
                    "binary-path": "/tmp/crash_recovery"
                }
            }
        });
        let mut packaged = BTreeMap::new();
        packaged.insert(PathBuf::from("/tmp/crash_recovery"), "abc".into());
        build::retain_packaged_inventory(&mut inventory, &packaged).unwrap();
        let binaries = inventory["rust-binaries"].as_object().unwrap();
        assert!(binaries.contains_key("kithara-assets::crash_recovery"));
        assert!(!binaries.contains_key("kithara-assets"));
        assert_eq!(inventory["rust-build-meta"]["keep"].as_bool(), Some(true));
    }

    #[test]
    fn apk_packaging_rejects_test_images_that_still_carry_the_static_graph() {
        assert!(build::is_shared_graph_library("libkithara_test_dylib.so"));
        assert!(build::is_shared_graph_library(
            "libkithara_test_dylib-0123abcd.so"
        ));
        assert!(!build::is_shared_graph_library("libkithara_test_abc.so"));
        let fat = build::oversized_test_libraries(&[
            ("libkithara_test_dylib.so".into(), 600 * 1024 * 1024),
            ("libkithara_test_abc.so".into(), 400 * 1024 * 1024),
            ("libstd-50ea.so".into(), 5 * 1024 * 1024),
        ]);
        assert_eq!(fat.len(), 1);
        assert_eq!(fat[0].0, "libkithara_test_abc.so");
    }
}
