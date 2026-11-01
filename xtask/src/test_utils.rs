use std::{
    env,
    fs::{self, File},
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use cargo_metadata::Message;
use kithara_devtools::Ctx;
use tracing::info;

use crate::{child, job};

struct Consumer<'a> {
    cancel: &'a child::Cancel,
    directory: &'a Path,
    capture: &'a Path,
    target: &'a Path,
}

pub(crate) fn run(ctx: &Ctx) -> Result<()> {
    ensure!(
        cfg!(any(target_os = "linux", target_os = "macos")),
        "test utilities check requires Linux or macOS"
    );
    let cancel = child::Cancel::install()?;
    let root = ctx
        .root
        .join(env::var_os("CARGO_TARGET_DIR").unwrap_or_else(|| "target".into()));
    fs::create_dir_all(&root)?;
    let source = tempfile::tempdir()?;
    let directory = source.path().join("consumer");
    let capture = source.path().join("capture");
    fs::create_dir(&directory)?;
    fs::create_dir(&capture)?;
    let target = tempfile::Builder::new()
        .prefix("test-utils-check-")
        .tempdir_in(&root)?;
    let consumer = Consumer {
        cancel: &cancel,
        directory: &directory,
        capture: &capture,
        target: target.path(),
    };
    consumer.prepare(&ctx.root)?;
    consumer.closure()?;
    consumer.capabilities()
}

impl Consumer<'_> {
    fn capabilities(&self) -> Result<()> {
        let toolchain = self.checked(Command::new("rustc").arg("-vV"), "toolchain", Some(30))?;
        info!(toolchain = toolchain.trim(), "test utilities toolchain");
        for offline in [false, true] {
            for features in ["", "mock", "multithread", "hang", "devtools"] {
                let label = format!("native-{features}-offline-{offline}");
                let report = self.checked(
                    self.cargo(&[
                        "test",
                        "--no-run",
                        "--message-format=json",
                        "--locked",
                        "--features",
                        features,
                    ])
                    .env("CARGO_NET_OFFLINE", offline.to_string()),
                    &format!("{label}-build"),
                    None,
                )?;
                let binary = test_binary(&report)?;
                let report = self.checked(&mut Command::new(&binary), &label, Some(300))?;
                info!(label, report = report.trim(), "native consumer passed");
                if !offline && features == "hang" {
                    self.watchdog(&binary)?;
                }
            }
            self.checked(
                self.cargo(&[
                    "check",
                    "--tests",
                    "--locked",
                    "--target",
                    "wasm32-unknown-unknown",
                    "--features",
                    "devtools,mock",
                ])
                .env("CARGO_NET_OFFLINE", offline.to_string()),
                &format!("wasm-compile-only-offline-{offline}"),
                None,
            )?;
        }
        Ok(())
    }

    fn cargo(&self, args: &[&str]) -> Command {
        let mut command = Command::new("cargo");
        command
            .current_dir(self.directory)
            .env("CARGO_TARGET_DIR", self.target)
            .args(args);
        for name in [
            "RUSTFLAGS",
            "CARGO_ENCODED_RUSTFLAGS",
            "RUSTDOCFLAGS",
            "RUSTC_WORKSPACE_WRAPPER",
        ] {
            command.env_remove(name);
        }
        command
    }

    fn checked(&self, command: &mut Command, label: &str, seconds: Option<u64>) -> Result<String> {
        let output = self.execute(command, label, seconds)?;
        ensure!(
            output.status.success(),
            "{label}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).with_context(|| format!("{label} stdout is not UTF-8"))
    }

    fn closure(&self) -> Result<()> {
        self.checked(
            self.cargo(&[
                "metadata",
                "--locked",
                "--all-features",
                "--format-version",
                "1",
            ])
            .env("CARGO_NET_OFFLINE", "true"),
            "metadata",
            Some(300),
        )?;
        let mut forbidden = Vec::new();
        for triple in [
            "x86_64-unknown-linux-gnu",
            "aarch64-apple-darwin",
            "wasm32-unknown-unknown",
        ] {
            for features in ["", "devtools", "mock,multithread,hang,devtools"] {
                let label = format!("closure-{triple}-{features}");
                let tree = self.checked(
                    self.cargo(&[
                        "tree",
                        "--locked",
                        "--edges",
                        "normal,build",
                        "--prefix",
                        "depth",
                        "--format",
                        "{p} {f}",
                        "--target",
                        triple,
                        "--features",
                        features,
                    ])
                    .env("CARGO_NET_OFFLINE", "true"),
                    &label,
                    Some(300),
                )?;
                forbidden.extend(forbidden_paths(&tree)?);
                info!(
                    label,
                    nodes = tree.lines().count(),
                    "normal/build closure includes host macros"
                );
            }
        }
        forbidden.sort();
        forbidden.dedup();
        ensure!(
            forbidden.is_empty(),
            "forbidden closure:\n{}",
            forbidden.join("\n")
        );
        Ok(())
    }

    fn execute(&self, command: &mut Command, label: &str, seconds: Option<u64>) -> Result<Output> {
        let stdout = self.capture.join(format!("{label}.stdout"));
        let stderr = self.capture.join(format!("{label}.stderr"));
        info!(label, ?command, "test utilities command");
        command
            .current_dir(self.directory)
            .stdin(Stdio::null())
            .stdout(File::create(&stdout)?)
            .stderr(File::create(&stderr)?);
        let mut process = child::spawn(command)?;
        let status = child::supervise(
            &mut process,
            Some(self.cancel),
            seconds.map(Duration::from_secs),
        );
        let stdout = fs::read(stdout)?;
        let stderr = fs::read(stderr)?;
        let status = status.with_context(|| {
            format!(
                "{label}: {}\n{}",
                String::from_utf8_lossy(&stdout),
                String::from_utf8_lossy(&stderr)
            )
        })?;
        Ok(Output {
            status,
            stdout,
            stderr,
        })
    }

    fn prepare(&self, root: &Path) -> Result<()> {
        let checkout = self.checked(
            Command::new("git")
                .arg("-C")
                .arg(root)
                .args(["rev-parse", "HEAD"]),
            "source-revision",
            Some(30),
        )?;
        let pin = match (
            env::var("KITHARA_TEST_UTILS_REV").ok(),
            env::var("KITHARA_TEST_UTILS_GIT_URL").ok(),
        ) {
            (Some(revision), Some(repository)) => {
                Some((job::repository_url(&repository)?, revision))
            }
            (None, None) => job::source_repository(&|name| env::var(name).ok())?
                .map(|repository| (repository, checkout.trim().to_owned())),
            _ => anyhow::bail!(
                "a Git pin requires KITHARA_TEST_UTILS_REV and KITHARA_TEST_UTILS_GIT_URL together"
            ),
        };
        if let Some((_, revision)) = &pin {
            ensure!(
                revision.len() == 40 && revision.bytes().all(|byte| byte.is_ascii_hexdigit()),
                "Git pin requires a full SHA"
            );
        }
        fs::create_dir_all(self.directory.join("src"))?;
        fs::copy(
            root.join("xtask/tests/fixtures/test_utils.rs"),
            self.directory.join("src/lib.rs"),
        )?;
        fs::write(
            self.directory.join("Cargo.toml"),
            manifest(
                root,
                pin.as_ref().map(|(url, rev)| (url.as_str(), rev.as_str())),
            )?,
        )?;
        info!(source = checkout.trim(), pin = ?pin, "test utilities");
        self.checked(
            self.cargo(&["generate-lockfile"])
                .env("CARGO_NET_GIT_FETCH_WITH_CLI", "true"),
            "prepare-lock",
            Some(300),
        )?;
        self.checked(&mut self.cargo(&["fetch", "--locked"]), "fetch", Some(300))?;
        Ok(())
    }

    fn watchdog(&self, binary: &Path) -> Result<()> {
        let output = self.execute(
            Command::new(binary)
                .args([
                    "--ignored",
                    "--exact",
                    "watchdog::hang_watchdog_fires",
                    "--nocapture",
                ])
                .env("KITHARA_HANG_DUMP_DIR", self.capture),
            "hang-child",
            Some(10),
        )?;
        let diagnostics = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        ensure!(
            !output.status.success()
                && diagnostics.contains("[HangDetector] `test-utils-watchdog` no progress for 1s")
                && diagnostics
                    .contains("[kithara_hang_detector] hang detected: test-utils-watchdog"),
            "watchdog did not reject the stuck child: {diagnostics}"
        );
        let dump = fs::read_dir(self.capture)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<std::io::Result<Vec<_>>>()?
            .into_iter()
            .find(|path| {
                path.file_name().is_some_and(|name| {
                    name.to_string_lossy()
                        .starts_with("kithara-hang-test-utils-watchdog-")
                })
            })
            .context("watchdog JSON dump")?;
        let dump: serde_json::Value = serde_json::from_slice(&fs::read(dump)?)?;
        ensure!(
            dump["schema"] == "kithara.hang.v1" && dump["label"] == "test-utils-watchdog",
            "unexpected watchdog dump: {dump}"
        );
        Ok(())
    }
}

fn manifest(root: &Path, pin: Option<(&str, &str)>) -> Result<String> {
    let workspace: toml::Value = toml::from_str(&fs::read_to_string(root.join("Cargo.toml"))?)?;
    let dependencies = workspace
        .get("workspace")
        .and_then(|workspace| workspace.get("dependencies"))
        .context("workspace dependencies")?;
    let mut manifest: toml::Value = toml::from_str(
        r#"
[workspace]
resolver = "2"
[workspace.dependencies]
[package]
name = "test-utils-check"
version = "0.0.0"
edition = "2024"
publish = false
[features]
default = []
mock = ["dep:unimock", "kithara-test-utils/mock"]
multithread = ["dep:kithara-platform", "kithara-platform/tokio-rt-multi-thread"]
hang = ["kithara-test-utils/hang"]
devtools = ["dep:kithara-config", "dep:kithara-derive"]
[dependencies]
kithara-config = { optional = true }
kithara-derive = { features = ["config"], optional = true }
kithara-test-utils = { default-features = false }
unimock = { workspace = true, optional = true }
[target.'cfg(not(target_arch = "wasm32"))'.dependencies]
kithara-platform = { optional = true }
[target.'cfg(target_arch = "wasm32")'.dependencies]
kithara-test-utils = { default-features = false, features = ["subscriber"] }
wasm-bindgen-test = { workspace = true }
"#,
    )?;
    let registry = manifest["workspace"]["dependencies"]
        .as_table_mut()
        .context("consumer workspace dependencies")?;
    for name in ["unimock", "wasm-bindgen-test"] {
        registry.insert(
            name.into(),
            dependencies
                .get(name)
                .context("workspace registry dependency")?
                .clone(),
        );
    }
    let replace = |consumer: &mut toml::Value| -> Result<()> {
        for (name, dependency) in consumer.as_table_mut().context("consumer dependencies")? {
            if name.starts_with("kithara-") {
                let table = dependency
                    .as_table_mut()
                    .context("Kithara dependency table")?;
                if let Some((url, revision)) = pin {
                    table.insert("git".into(), url.into());
                    table.insert("rev".into(), revision.into());
                } else {
                    let relative = dependencies
                        .get(name)
                        .and_then(|dependency| dependency.get("path"))
                        .and_then(toml::Value::as_str)
                        .with_context(|| format!("workspace dependency path for {name}"))?;
                    let path = root.join(relative).canonicalize()?;
                    table.insert(
                        "path".into(),
                        path.to_str().context("UTF-8 dependency path")?.into(),
                    );
                }
            }
        }
        Ok(())
    };
    replace(&mut manifest["dependencies"])?;
    for (_, target) in manifest["target"]
        .as_table_mut()
        .context("consumer target dependencies")?
    {
        replace(&mut target["dependencies"])?;
    }
    toml::to_string(&manifest).context("serialize consumer manifest")
}

fn test_binary(report: &str) -> Result<PathBuf> {
    let binaries = Message::parse_stream(report.as_bytes())
        .filter_map(|message| match message {
            Ok(Message::CompilerArtifact(artifact)) if artifact.profile.test => {
                artifact.executable.map(|path| Ok(path.into_std_path_buf()))
            }
            Ok(_) => None,
            Err(error) => Some(Err(error)),
        })
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let [binary] = binaries.as_slice() else {
        anyhow::bail!("expected one consumer test executable, got {binaries:?}")
    };
    Ok(binary.clone())
}

fn forbidden_paths(tree: &str) -> Result<Vec<String>> {
    ensure!(!tree.is_empty(), "empty dependency closure");
    let mut path = Vec::new();
    let mut forbidden = Vec::new();
    for line in tree.lines() {
        let digits = line.bytes().take_while(u8::is_ascii_digit).count();
        let depth = line[..digits]
            .parse::<usize>()
            .context("cargo tree depth")?;
        let package = line[digits..]
            .split_whitespace()
            .next()
            .context("cargo tree package")?;
        ensure!(
            depth <= path.len(),
            "cargo tree has no parent at depth {depth}"
        );
        path.truncate(depth);
        path.push(package);
        if matches!(
            package,
            "kithara-workspace-hack"
                | "kithara-net"
                | "kithara-events"
                | "kithara-audio"
                | "kithara-play"
                | "reqwest"
                | "wreq"
                | "boring"
                | "boring2"
                | "cpal"
                | "firewheel"
                | "usdt"
        ) {
            forbidden.push(path.join(" -> "));
        }
    }
    Ok(forbidden)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use crate::testing::signals;

    #[cfg(unix)]
    #[test]
    fn direct_children_keep_the_consumer_and_compiler_cache_setup() -> Result<()> {
        let _signals = signals();
        let cancel = child::Cancel::install()?;
        let root = tempfile::tempdir()?;
        let directory = root.path().join("consumer");
        let capture = root.path().join("capture");
        fs::create_dir(&directory)?;
        fs::create_dir(&capture)?;
        let consumer = Consumer {
            cancel: &cancel,
            directory: &directory,
            capture: &capture,
            target: &root.path().join("target"),
        };
        let cwd = consumer.checked(&mut Command::new("pwd"), "cwd", Some(10))?;
        assert_eq!(
            Path::new(cwd.trim()).canonicalize()?,
            directory.canonicalize()?
        );
        assert!(
            !consumer
                .cargo(&["check"])
                .get_envs()
                .any(|(name, value)| name == "RUSTC_WRAPPER" && value.is_none()),
            "nested native and WASM compilations must inherit the compiler cache"
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn failures_include_output_after_temporary_capture_is_removed() -> Result<()> {
        let _signals = signals();
        let cancel = child::Cancel::install()?;
        for (script, seconds) in [
            ("printf stdout; printf stderr >&2; exit 7", None),
            ("printf stdout; printf stderr >&2; exec sleep 60", Some(1)),
        ] {
            let root = tempfile::tempdir()?;
            let capture = root.path().join("capture");
            fs::create_dir(&capture)?;
            let consumer = Consumer {
                cancel: &cancel,
                directory: root.path(),
                capture: &capture,
                target: &root.path().join("target"),
            };
            let error = consumer
                .checked(Command::new("sh").args(["-c", script]), "failure", seconds)
                .expect_err("command must fail");
            drop(root);
            assert!(!capture.exists());
            let diagnostics = format!("{error:#}");
            assert!(diagnostics.contains("stdout"), "{diagnostics}");
            assert!(diagnostics.contains("stderr"), "{diagnostics}");
        }
        Ok(())
    }

    #[test]
    fn closure_oracle_retains_host_backend_paths() {
        assert_eq!(
            forbidden_paths("0consumer v0.0.0\n1host-macro v0.1.0 (proc-macro)\n2cpal v0.17.0\n")
                .unwrap(),
            ["consumer -> host-macro -> cpal"]
        );
        for invalid in ["", "not a tree", "2orphan v0.1.0\n"] {
            assert!(forbidden_paths(invalid).is_err());
        }
    }

    #[test]
    fn independent_manifest_pins_all_kithara_dependencies() -> Result<()> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let rendered = manifest(
            root,
            Some((
                "https://github.example/owner/repo.git",
                "0123456789abcdef0123456789abcdef01234567",
            )),
        )?;
        assert_eq!(rendered.matches("git = ").count(), 5);
        assert_eq!(rendered.matches("rev = ").count(), 5);
        assert!(!rendered.contains("path = "));
        let parsed: toml::Value = toml::from_str(&rendered)?;
        assert_eq!(parsed["workspace"]["resolver"].as_str(), Some("2"));
        assert_eq!(
            parsed["dependencies"]["kithara-config"]["optional"].as_bool(),
            Some(true)
        );
        assert_eq!(
            parsed["dependencies"]["kithara-derive"]["features"][0].as_str(),
            Some("config")
        );
        assert_eq!(
            parsed["dependencies"]["kithara-test-utils"]["default-features"].as_bool(),
            Some(false)
        );
        let wasm = &parsed["target"]["cfg(target_arch = \"wasm32\")"]["dependencies"];
        assert_eq!(
            wasm["kithara-test-utils"]["features"][0].as_str(),
            Some("subscriber")
        );
        assert_eq!(
            wasm["kithara-test-utils"]["default-features"].as_bool(),
            Some(false)
        );
        assert_eq!(
            parsed["dependencies"]["unimock"]["workspace"].as_bool(),
            Some(true)
        );
        let workspace: toml::Value = toml::from_str(&fs::read_to_string(root.join("Cargo.toml"))?)?;
        for name in ["unimock", "wasm-bindgen-test"] {
            assert_eq!(
                parsed["workspace"]["dependencies"][name],
                workspace["workspace"]["dependencies"][name]
            );
        }
        let local: toml::Value = toml::from_str(&manifest(root, None)?)?;
        let path = local["dependencies"]["kithara-test-utils"]["path"]
            .as_str()
            .unwrap();
        assert!(Path::new(path).is_absolute());
        Ok(())
    }

    #[test]
    fn manifest_rejects_missing_workspace_paths() -> Result<()> {
        let root = tempfile::tempdir()?;
        fs::write(
            root.path().join("Cargo.toml"),
            "[workspace.dependencies]\nunimock = \"0.6\"\nwasm-bindgen-test = \"0.3\"\nkithara-config = { version = \"0.0.0\" }\n",
        )?;
        let error = manifest(root.path(), None).expect_err("missing dependency path");
        assert_eq!(
            error.to_string(),
            "workspace dependency path for kithara-config"
        );
        Ok(())
    }

    #[test]
    fn test_executable_is_selected_from_cargo_artifacts() -> Result<()> {
        let artifact = serde_json::json!({
            "reason": "compiler-artifact", "package_id": "path+file:///consumer#test-utils-check@0.0.0",
            "manifest_path": "/consumer/Cargo.toml", "features": [], "filenames": [], "fresh": false,
            "target": { "kind": ["lib"], "crate_types": ["lib"], "name": "test_utils_check", "src_path": "/consumer/src/lib.rs", "edition": "2024", "doc": true, "doctest": false, "test": true },
            "profile": { "opt_level": "0", "debuginfo": 2, "debug_assertions": true, "overflow_checks": true, "test": true },
            "executable": "/consumer/target/test-utils-check"
        });
        let report = artifact.to_string();
        assert_eq!(
            test_binary(&report)?,
            PathBuf::from("/consumer/target/test-utils-check")
        );
        assert!(test_binary("").is_err());
        assert!(test_binary(&format!("{report}\n{report}")).is_err());
        Ok(())
    }
}
