#[cfg(unix)]
use std::{
    env,
    process::{Command, Output},
    time::Duration,
};
use std::{
    fs,
    path::{Path, PathBuf},
};

#[cfg(unix)]
use anyhow::Context;
use anyhow::Result;

#[cfg(unix)]
use super::consumer::Consumer;
use super::consumer::{forbidden_paths, manifest, test_binary};
#[cfg(unix)]
use crate::child;
#[cfg(unix)]
use crate::testing::{install_double, install_scripted_double, signals};

#[cfg(unix)]
#[test]
#[ignore = "invoked by the instrumentation regression parents"]
fn instrumentation_fixture_child() -> Result<()> {
    let _signals = signals();
    let cancel = child::Cancel::install()?;
    let root = PathBuf::from(
        env::var_os("KITHARA_TEST_UTILS_INSTRUMENTATION_ROOT")
            .context("instrumentation regression root")?,
    );
    let consumer = Consumer {
        cancel: &cancel,
        directory: &root.join("consumer"),
        capture: &root.join("capture"),
        target: &root.join("target"),
    };
    let refused = child::output(
        consumer.cargo(&["run"]).env_remove("KITHARA_TEST_TRACE"),
        Some(&cancel),
        Duration::from_secs(300),
    )?;
    assert_eq!(
        refused.status.code(),
        Some(97),
        "fake Cargo must refuse run"
    );
    match env::var("KITHARA_TEST_UTILS_CASE").as_deref() {
        Ok("native") => consumer.capabilities(),
        Ok("watchdog") => consumer.watchdog(&test_binary(&fs::read_to_string(
            consumer.directory.join("report.json"),
        )?)?),
        _ => consumer.instrumentation(),
    }
}

#[cfg(unix)]
fn dynamic_artifact(consumer: &Consumer<'_>, tests: bool) -> Result<PathBuf> {
    let directory = consumer.directory;
    let target = consumer.target.join("debug");
    let deps = target.join("deps");
    fs::create_dir_all(&deps)?;
    fs::create_dir(target.join("examples"))?;
    fs::write(
        directory.join("library.rs"),
        "pub fn answer() -> u32 { 42 }",
    )?;
    let library = deps.join(format!("libloader_contract{}", env::consts::DLL_SUFFIX));
    consumer.checked(
        Command::new("rustc")
            .args([
                "--crate-name",
                "loader_contract",
                "--crate-type",
                "dylib",
                "-C",
                "prefer-dynamic",
                "library.rs",
                "-o",
            ])
            .arg(&library),
        "dynamic-library-build",
        Some(300),
    )?;
    let source = if tests {
        r#"
use std::io::Write as _;
#[test]
fn native_consumer() -> std::io::Result<()> {
    assert_eq!(loader_contract::answer(), 42);
    std::fs::OpenOptions::new().create(true).append(true).open("runtime-runs")?.write_all(b"native\n")
}
mod watchdog {
    use super::*;
    #[test]
    #[ignore]
    fn hang_watchdog_fires() -> std::io::Result<()> {
        assert_eq!(loader_contract::answer(), 42);
        std::fs::OpenOptions::new().create(true).append(true).open("runtime-runs")?.write_all(b"watchdog\n")?;
        let capture = std::env::var_os("KITHARA_HANG_DUMP_DIR").unwrap();
        std::fs::write(std::path::PathBuf::from(capture).join("kithara-hang-test-utils-watchdog-fixture.json"),
            "{\"schema\":\"kithara.hang.v1\",\"label\":\"test-utils-watchdog\"}")?;
        eprintln!("[HangDetector] `test-utils-watchdog` no progress for 1s");
        eprintln!("[kithara_hang_detector] hang detected: test-utils-watchdog");
        std::process::exit(7)
    }
}
"#
    } else {
        r#"
use std::io::Write as _;
fn main() -> std::io::Result<()> {
    assert_eq!(loader_contract::answer(), 42);
    std::fs::OpenOptions::new().create(true).append(true).open("runtime-runs")?.write_all(b"artifact\n")?;
    println!("runtime stdout");
    eprintln!("runtime stderr");
    Ok(())
}
"#
    };
    fs::write(directory.join("main.rs"), source)?;
    let executable = target.join(if tests {
        "deps/native-consumer"
    } else {
        "examples/instrumentation"
    });
    let mut compiler = Command::new("rustc");
    compiler
        .args([
            "--edition=2024",
            "-C",
            "prefer-dynamic",
            "main.rs",
            "--extern",
        ])
        .arg(format!("loader_contract={}", library.display()))
        .arg("-L")
        .arg(format!("dependency={}", deps.display()))
        .arg("-o")
        .arg(&executable);
    if tests {
        compiler.arg("--test");
    }
    consumer.checked(&mut compiler, "dynamic-example-build", Some(300))?;
    let direct = consumer.execute(
        Command::new(&executable)
            .env_remove("LD_LIBRARY_PATH")
            .env_remove("DYLD_FALLBACK_LIBRARY_PATH"),
        "dynamic-unconfigured-run",
        Some(300),
    )?;
    assert!(
        !direct.status.success(),
        "the fixture must require a loader path"
    );
    assert!(
        !directory.join("runtime-runs").exists(),
        "the loader must fail before main"
    );
    let diagnostics = String::from_utf8_lossy(&direct.stderr);
    assert!(
        diagnostics.contains("libstd") || diagnostics.contains("loader_contract"),
        "{diagnostics}"
    );
    Ok(executable)
}

#[cfg(unix)]
fn compiler_environment(
    consumer: &Consumer<'_>,
    bin: &Path,
    case: &str,
    command: &mut Command,
) -> Result<()> {
    let libdir = consumer.checked(
        Command::new("rustc").args(["--print", "target-libdir"]),
        "fixture-libdir",
        Some(30),
    )?;
    fs::write(consumer.directory.join("compiler-libdir"), libdir)?;
    install_double(bin, "rustc");
    let selected = install_scripted_double(
        bin,
        "selected-rustc",
        "printf 'selected\n' >> compiler.calls\ncat compiler-libdir\n",
    );
    let rejected = install_scripted_double(
        bin,
        "rejected-rustc",
        "printf 'rejected\n' >> rejected.calls\nprintf 'selected compiler failed\n' >&2\nexit 91\n",
    );
    match case {
        "compiler-rustc" => {
            command.env("RUSTC", &selected);
        }
        "compiler-build" => {
            command.env("CARGO_BUILD_RUSTC", &selected);
        }
        "compiler-priority" => {
            command
                .env("RUSTC", &selected)
                .env("CARGO_BUILD_RUSTC", &rejected);
        }
        "compiler-fails" => {
            command
                .env("RUSTC", &rejected)
                .env("CARGO_BUILD_RUSTC", &selected);
        }
        _ => anyhow::bail!("unknown compiler fixture case: {case}"),
    }
    Ok(())
}

#[cfg(unix)]
fn fixture_output(root: &Path, bin: &Path, case: &str, cancel: &child::Cancel) -> Result<Output> {
    let directory = root.join("consumer");
    let path = env::join_paths(
        std::iter::once(bin.to_path_buf())
            .chain(env::split_paths(&env::var_os("PATH").unwrap_or_default())),
    )?;
    let mut command = Command::new(env::current_exe()?);
    command.args(["--exact", "test_utils::tests::instrumentation_fixture_child", "--ignored", "--nocapture"])
        .env("PATH", path)
        .env_remove("LD_LIBRARY_PATH")
        .env_remove("DYLD_FALLBACK_LIBRARY_PATH")
        .env_remove("RUSTC")
        .env_remove("CARGO_BUILD_RUSTC")
        .env("KITHARA_TEST_UTILS_INSTRUMENTATION_ROOT", root)
        .env("KITHARA_TEST_UTILS_CASE", case)
        .env("KITHARA_TEST_STDOUT", fs::read_to_string(directory.join("report.json"))?)
        .env("KITHARA_TEST_TRACE", directory.join("cargo.calls"))
        .env("KITHARA_TEST_RULES", "cargo:build:*=0,cargo:test:*=0,cargo:check:*=0,cargo:run:*=97,rustc:*:*=96,cargo:*:*=98");
    if case.starts_with("compiler-") {
        compiler_environment(
            &Consumer {
                cancel,
                directory: &directory,
                capture: &root.join("capture"),
                target: &root.join("target"),
            },
            bin,
            case,
            &mut command,
        )?;
    }
    child::output(&mut command, Some(cancel), Duration::from_secs(300))
}

#[cfg(unix)]
fn instrumentation_fixture(case: &str) -> Result<(tempfile::TempDir, Output)> {
    let _signals = signals();
    let cancel = child::Cancel::install()?;
    let root = tempfile::tempdir()?;
    let directory = root.path().join("consumer");
    let bin = root.path().join("bin");
    fs::create_dir(&directory)?;
    fs::create_dir(root.path().join("capture"))?;
    let executable =
        if matches!(case, "dynamic" | "native" | "watchdog") || case.starts_with("compiler-") {
            dynamic_artifact(
                &Consumer {
                    cancel: &cancel,
                    directory: &directory,
                    capture: &root.path().join("capture"),
                    target: &root.path().join("target"),
                },
                matches!(case, "native" | "watchdog"),
            )?
        } else {
            install_scripted_double(
                &bin,
                "instrumentation",
                r#"set -eu
test "$(pwd -P)" = "$(cat expected-cwd)"
printf 'artifact\n' >> runtime-runs
printf 'runtime stdout\n'
printf 'runtime stderr\n' >&2
if test -f runtime-fails; then exit 7; fi
"#,
            )
        };
    fs::write(
        directory.join("expected-cwd"),
        directory
            .canonicalize()?
            .to_str()
            .context("UTF-8 fixture cwd")?,
    )?;
    let library = root.path().join("target/deps").join(format!(
        "libkithara_test_macros-fixture{}",
        env::consts::DLL_SUFFIX
    ));
    let example = serde_json::json!({
        "reason": "compiler-artifact", "package_id": "path+file:///consumer#test-utils-check@0.0.0",
        "manifest_path": "/consumer/Cargo.toml", "features": [], "fresh": false,
        "filenames": [executable], "executable": executable,
        "target": { "kind": ["example"], "crate_types": ["bin"], "name": "instrumentation", "src_path": "/consumer/examples/instrumentation.rs", "edition": "2024", "doc": false, "doctest": false, "test": false },
        "profile": { "opt_level": "0", "debuginfo": 2, "debug_assertions": true, "overflow_checks": true, "test": false }
    });
    let mut macros = example.clone();
    macros["target"]["name"] = "kithara_test_macros".into();
    macros["target"]["kind"] = serde_json::json!(["proc-macro"]);
    macros["target"]["crate_types"] = serde_json::json!(["proc-macro"]);
    macros["filenames"] = serde_json::json!([library]);
    macros["executable"] = serde_json::Value::Null;
    let mut records = vec![macros.clone(), example.clone()];
    if matches!(case, "native" | "watchdog") {
        records[1]["profile"]["test"] = true.into();
        records[1]["target"]["kind"] = serde_json::json!(["lib"]);
    }
    let mut inputs = "/source/src/lib.rs".to_owned();
    match case {
        "missing-example" => {
            records.pop();
        }
        "duplicate-example" => records.push(example),
        "missing-executable" => records[1]["executable"] = serde_json::Value::Null,
        "wrong-target" => records[1]["target"]["name"] = "other".into(),
        "wrong-kind" => records[1]["target"]["kind"] = serde_json::json!(["bin"]),
        "mixed-kind" => records[1]["target"]["kind"] = serde_json::json!(["example", "bin"]),
        "test-profile" => records[1]["profile"]["test"] = true.into(),
        "duplicate-macros" => records.push(macros),
        "missing-macros" => {
            records.remove(0);
        }
        "test-args" => {
            let mut forbidden = macros;
            forbidden["target"]["name"] = "kithara_test_args".into();
            forbidden["target"]["kind"] = serde_json::json!(["lib"]);
            forbidden["target"]["crate_types"] = serde_json::json!(["lib"]);
            records.push(forbidden);
        }
        "test-input" => inputs.push_str(" /source/src/test/compiler.rs"),
        "asset-input" => inputs.push_str(" /source/src/asset/compiler.rs"),
        "fixture-input" => inputs.push_str(" /source/src/fixture.rs"),
        "missing-input" => inputs = "/source/src/other.rs".into(),
        "launch" => records[1]["executable"] = serde_json::json!(bin.join("missing")),
        "nonzero" => fs::write(directory.join("runtime-fails"), "")?,
        "ok" | "dynamic" | "native" | "watchdog" | "compiler-rustc" | "compiler-build"
        | "compiler-priority" | "compiler-fails" | "malformed" | "trailing-garbage" => {}
        _ => anyhow::bail!("unknown instrumentation regression case: {case}"),
    }
    let mut lines = records.iter().map(ToString::to_string).collect::<Vec<_>>();
    if case == "trailing-garbage" {
        lines[1].push_str(" trailing garbage");
    }
    lines.push(serde_json::json!({"reason": "build-finished", "success": true}).to_string());
    let mut report = lines.join("\n");
    if case == "malformed" {
        report.push_str("\n{");
    }
    fs::write(directory.join("report.json"), report)?;
    fs::create_dir_all(library.parent().context("macro fixture directory")?)?;
    fs::write(
        library.with_file_name("kithara_test_macros-fixture.d"),
        inputs,
    )?;
    install_double(&bin, "cargo");
    let output = fixture_output(root.path(), &bin, case, &cancel)?;
    Ok((root, output))
}

#[cfg(unix)]
#[test]
fn native_consumers_run_dynamically_linked_artifacts() -> Result<()> {
    let (root, output) = instrumentation_fixture("native")?;
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let runs = fs::read_to_string(root.path().join("consumer/runtime-runs"))?;
    assert_eq!(runs.lines().filter(|line| *line == "native").count(), 18);
    assert_eq!(runs.lines().filter(|line| *line == "watchdog").count(), 1);
    assert_eq!(
        fs::read_to_string(root.path().join("consumer/cargo.calls"))?
            .lines()
            .count(),
        22
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn watchdog_runs_the_dynamically_linked_test_artifact() -> Result<()> {
    let (root, output) = instrumentation_fixture("watchdog")?;
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(root.path().join("consumer/runtime-runs"))?,
        "watchdog\n"
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn instrumentation_uses_the_cargo_selected_compiler() -> Result<()> {
    for case in ["compiler-rustc", "compiler-build", "compiler-priority"] {
        let (root, output) = instrumentation_fixture(case)?;
        assert!(
            output.status.success(),
            "{case}: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            fs::read_to_string(root.path().join("consumer/runtime-runs"))?,
            "artifact\n".repeat(3)
        );
        assert!(!fs::read_to_string(root.path().join("consumer/compiler.calls"))?.is_empty());
        assert!(!root.path().join("consumer/rejected.calls").exists());
    }
    let (root, output) = instrumentation_fixture("compiler-fails")?;
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("selected compiler failed"));
    assert!(!root.path().join("consumer/runtime-runs").exists());
    assert!(!root.path().join("consumer/compiler.calls").exists());
    assert_eq!(
        fs::read_to_string(root.path().join("consumer/rejected.calls"))?,
        "rejected\n"
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn instrumentation_runs_build_artifact_in_consumer_without_cargo_run() -> Result<()> {
    let (root, output) = instrumentation_fixture("ok")?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let directory = root.path().join("consumer");
    let calls = fs::read_to_string(directory.join("cargo.calls"))?;
    let calls = calls.lines().collect::<Vec<_>>();
    assert_eq!(calls.len(), 3);
    for (features, call) in ["", "facade", "instrumented"].into_iter().zip(calls) {
        let args = call.split(' ').collect::<Vec<_>>();
        assert_eq!(args[0], "build");
        assert!(args.contains(&"--locked"));
        assert!(args.contains(&"--message-format=json"));
        for pair in [["--example", "instrumentation"], ["--features", features]] {
            assert!(args.windows(2).any(|args| args == pair), "{call}");
        }
        let label = format!("instrumentation-run-{features}");
        assert_eq!(
            fs::read_to_string(root.path().join("capture").join(format!("{label}.stdout")))?,
            "runtime stdout\n"
        );
        assert_eq!(
            fs::read_to_string(root.path().join("capture").join(format!("{label}.stderr")))?,
            "runtime stderr\n"
        );
    }
    assert_eq!(
        fs::read_to_string(directory.join("runtime-runs"))?,
        "artifact\n".repeat(3)
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn instrumentation_runs_dynamically_linked_artifacts() -> Result<()> {
    let (root, output) = instrumentation_fixture("dynamic")?;
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(root.path().join("consumer/runtime-runs"))?,
        "artifact\n".repeat(3)
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn instrumentation_rejects_invalid_build_artifacts_and_failed_children() -> Result<()> {
    for (case, expected) in [
        ("missing-example", "instrumentation example artifact"),
        (
            "duplicate-example",
            "multiple instrumentation example executables",
        ),
        ("missing-executable", "instrumentation example executable"),
        ("wrong-target", "instrumentation example artifact"),
        (
            "wrong-kind",
            "instrumentation artifact is not a runnable example",
        ),
        (
            "mixed-kind",
            "instrumentation artifact is not a runnable example",
        ),
        (
            "test-profile",
            "instrumentation artifact is not a runnable example",
        ),
        ("malformed", "instrumentation Cargo message is not JSON"),
        (
            "trailing-garbage",
            "instrumentation Cargo message is not JSON",
        ),
        ("missing-macros", "instrumentation macro artifact"),
        ("duplicate-macros", "multiple instrumentation macro units"),
        (
            "test-args",
            "test argument compiler reached production instrumentation",
        ),
        (
            "test-input",
            "test-only compiler inputs reached production instrumentation",
        ),
        (
            "asset-input",
            "test-only compiler inputs reached production instrumentation",
        ),
        (
            "fixture-input",
            "test-only compiler inputs reached production instrumentation",
        ),
        ("missing-input", "missing macro source inputs"),
        ("launch", "spawning owned command"),
        ("nonzero", "instrumentation-run-"),
    ] {
        let (root, output) = instrumentation_fixture(case)?;
        assert!(!output.status.success(), "{case}");
        let diagnostics = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(diagnostics.contains(expected), "{case}: {diagnostics}");
        let directory = root.path().join("consumer");
        assert_eq!(
            fs::read_to_string(directory.join("cargo.calls"))?
                .lines()
                .count(),
            1,
            "{case}"
        );
        if case == "nonzero" {
            assert!(diagnostics.contains("runtime stdout"), "{diagnostics}");
            assert!(diagnostics.contains("runtime stderr"), "{diagnostics}");
            assert_eq!(
                fs::read_to_string(directory.join("runtime-runs"))?,
                "artifact\n"
            );
        } else {
            assert!(!directory.join("runtime-runs").exists(), "{case}");
        }
    }
    Ok(())
}

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
    // The killed case runs last: the command cancels this test process
    // only after writing, so the capture holds output when the owned
    // group is stopped, and the cancellation stays pending afterwards.
    for (script, killed) in [
        ("printf stdout; printf stderr >&2; exit 7", false),
        (
            "printf stdout; printf stderr >&2; kill -TERM $PPID; exec sleep 60",
            true,
        ),
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
            .checked(Command::new("sh").args(["-c", script]), "failure", None)
            .expect_err("command must fail");
        drop(root);
        assert!(!capture.exists());
        let diagnostics = format!("{error:#}");
        assert!(diagnostics.contains("stdout"), "{diagnostics}");
        assert!(diagnostics.contains("stderr"), "{diagnostics}");
        assert_eq!(diagnostics.contains("cancelled"), killed, "{diagnostics}");
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
    let root = crate::testing::workspace::root();
    let rendered = manifest(
        &root,
        Some((
            "https://github.example/owner/repo.git",
            "0123456789abcdef0123456789abcdef01234567",
        )),
    )?;
    assert_eq!(rendered.matches("git = ").count(), 6);
    assert_eq!(rendered.matches("rev = ").count(), 6);
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
    let local: toml::Value = toml::from_str(&manifest(&root, None)?)?;
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
    assert_eq!(error.to_string(), "workspace dependency path for kithara");
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
