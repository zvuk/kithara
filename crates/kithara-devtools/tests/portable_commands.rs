#![cfg(unix)]

//! Public command dispatch from an unrelated workspace with isolated child tools.

use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
};

use clap::{FromArgMatches, Subcommand};
use kithara_devtools::{CoreCommand, Ctx, verdict::NotClean};
use serde_json::{Value, json};
use tempfile::{TempDir, tempdir};

struct Probe {
    _directory: TempDir,
    selected: PathBuf,
    unrelated: PathBuf,
    before: BTreeMap<PathBuf, Vec<u8>>,
    capture: PathBuf,
    result: Value,
}

fn workspace(root: &Path, name: &str, config: &str) {
    fs::create_dir_all(root.join("src")).expect("create source directory");
    fs::create_dir_all(root.join(".config")).expect("create config directory");
    fs::write(
        root.join("Cargo.toml"),
        format!(
            "[package]\nname='{name}'\nversion='0.1.0'\nedition='2024'\nlicense='MIT'\n\
             [workspace]\n[workspace.package]\nlicense='MIT'\n"
        ),
    )
    .expect("write manifest");
    fs::write(root.join("src/lib.rs"), "").expect("write source");
    fs::write(root.join(".config/xtask.toml"), config).expect("write project config");
    fs::write(root.join(".config/nextest.toml"), "[profile.default]\n")
        .expect("write runner config");
}

fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut files = BTreeMap::new();
    for entry in fs::read_dir(root).expect("read fixture directory") {
        let path = entry.expect("read fixture entry").path();
        if path.is_dir() {
            files.extend(snapshot(&path));
        } else {
            files.insert(path.clone(), fs::read(path).expect("read fixture file"));
        }
    }
    files
}

fn probe(args: &[&str], selected_config: &str, code: i32) -> Probe {
    let directory = tempdir().expect("create command fixture");
    let selected = directory.path().join("selected");
    let unrelated = directory.path().join("unrelated");
    let bin = directory.path().join("bin");
    fs::create_dir(&bin).expect("create tool directory");
    let cargo = env!("CARGO").replace('\'', "'\\''");
    let script = format!(
        "#!/bin/sh\nif [ \"$1\" = metadata ]; then exec '{cargo}' \"$@\"; fi\n\
         printf '%s|%s|%s\\n' \"$PWD\" \"$0\" \"$*\" >> \"$PORTABLE_CAPTURE\"\n\
         exit \"$PORTABLE_EXIT_CODE\"\n"
    );
    for name in ["cargo", "selected-cargo"] {
        let path = bin.join(name);
        fs::write(&path, &script).expect("write deterministic child tool");
        fs::set_permissions(path, fs::Permissions::from_mode(0o755))
            .expect("make child executable");
    }
    let selected_cargo = bin.join("selected-cargo");
    let config = format!(
        "[project]\nname='external-command-consumer'\n\
         [tools.cargo]\nprogram={:?}\n{selected_config}",
        selected_cargo.to_string_lossy()
    );
    workspace(&selected, "external-command-consumer", &config);
    workspace(
        &unrelated,
        "unrelated-consumer",
        "[health]\ngeiger_package='unrelated-consumer'\n",
    );
    fs::create_dir(unrelated.join(".cargo")).expect("caller cargo directory");
    fs::write(unrelated.join(".cargo/config.toml"), "broken = [\n")
        .expect("unrelated Cargo config must not be read");
    let selected = selected.canonicalize().expect("canonical target workspace");
    let unrelated = unrelated
        .canonicalize()
        .expect("canonical caller workspace");
    if args.contains(&"style") || args == ["fixture", "lint"] {
        let dir = selected.join(".config/style");
        fs::create_dir_all(&dir).expect("create target style directory");
        fs::write(dir.join("thresholds.toml"), "unknown_portable_config = [")
            .expect("write malformed target policy");
    }
    let before = snapshot(&unrelated);
    let capture = directory.path().join("capture.txt");
    let result_path = directory.path().join("result.json");
    let current_path = std::env::var_os("PATH").expect("PATH exists");
    let paths = std::env::split_paths(&current_path);
    let path = std::env::join_paths(std::iter::once(bin).chain(paths)).expect("join child PATH");
    let output = Command::new(std::env::current_exe().expect("locate fixture adapter"))
        .args([
            "--exact",
            "portable_command_child",
            "--ignored",
            "--nocapture",
        ])
        .current_dir(&unrelated)
        .env("PATH", path)
        .env("PORTABLE_ROOT", &selected)
        .env(
            "PORTABLE_ARGS",
            serde_json::to_string(args).expect("encode command args"),
        )
        .env("PORTABLE_RESULT", &result_path)
        .env("PORTABLE_CAPTURE", &capture)
        .env("PORTABLE_EXIT_CODE", code.to_string())
        .env_remove("KITHARA_PIPELINE_KIND")
        .output()
        .expect("run public adapter in unrelated cwd");
    assert!(
        output.status.success(),
        "adapter fixture failed: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let result = serde_json::from_slice(&fs::read(result_path).expect("adapter records result"))
        .expect("decode adapter result");
    Probe {
        _directory: directory,
        selected,
        unrelated,
        before,
        capture,
        result,
    }
}

#[test]
fn clippy_honors_selected_workspace_tool_and_neutral_features() {
    let probe = probe(&["fixture", "clippy"], "", 41);
    assert_eq!(probe.result["exit_code"], 41);
    let captured = fs::read_to_string(&probe.capture).expect("child tool was invoked");
    assert!(captured.starts_with(&format!("{}|", probe.selected.display())));
    assert!(captured.contains("selected-cargo|clippy --workspace --all-targets -- -D warnings"));
    assert!(!captured.contains("kithara-ffi"));
    assert_eq!(snapshot(&probe.unrelated), probe.before);
}

#[test]
fn test_honors_target_lane_and_child_directory() {
    let probe = probe(
        &["fixture", "test", "--lane=portable", "--flash=off"],
        "[test]\ndefault_lane='portable'\ndefault_backend='none'\nnextest_config='.config/nextest.toml'\n\
         [test.net_backends.none]\nfeatures=[]\n\
         [test.lanes.portable.cargo]\npackages=['external-command-consumer']\n\
         [test.lanes.portable.runner.cargo]\n",
        41,
    );
    assert_eq!(probe.result["exit_code"], 41, "{}", probe.result);
    let captured = fs::read_to_string(&probe.capture).expect("target test child ran");
    assert!(captured.starts_with(&format!("{}|", probe.selected.display())));
    assert!(
        captured.contains("test ") && captured.contains("external-command-consumer"),
        "{captured}"
    );
    assert_eq!(snapshot(&probe.unrelated), probe.before);
}

#[test]
fn health_keeps_metadata_outputs_and_children_in_the_selected_workspace() {
    let probe = probe(
        &["fixture", "health"],
        "[health]\ngeiger_package='external-command-consumer'\n",
        0,
    );
    assert_eq!(probe.result["ok"], true, "{}", probe.result);
    let captured = fs::read_to_string(&probe.capture).expect("health children ran");
    assert!(
        captured
            .lines()
            .all(|line| line.starts_with(&format!("{}|", probe.selected.display())))
    );
    assert!(
        captured.contains(&format!(
            "selected-cargo|machete {}",
            probe.selected.join("Cargo.toml").display()
        )),
        "{captured}"
    );
    assert!(probe.selected.join("target/health-report.md").is_file());
    assert!(probe.selected.join("target/health-logs").is_dir());
    assert_eq!(snapshot(&probe.unrelated), probe.before);
}

#[test]
fn lint_rejects_the_selected_policy_without_touching_the_callers_workspace() {
    let probe = probe(
        &["fixture", "lint", "style", "--check", "readme_shape"],
        "",
        0,
    );
    assert_eq!(probe.result["ok"], false);
    assert!(
        probe.result["error"]
            .as_str()
            .expect("error string")
            .contains(
                &probe
                    .selected
                    .join(".config/style/thresholds.toml")
                    .display()
                    .to_string()
            )
    );
    assert_eq!(snapshot(&probe.unrelated), probe.before);
}

#[test]
fn aggregate_lint_keeps_the_selected_configuration_error() {
    let probe = probe(&["fixture", "lint"], "", 0);
    assert_eq!(probe.result["ok"], false);
    let error = probe.result["error"].as_str().expect("error string");
    assert!(
        error.contains(
            &probe
                .selected
                .join(".config/style/thresholds.toml")
                .display()
                .to_string()
        ),
        "{error}"
    );
    assert!(error.contains("parse"), "{error}");
    assert_eq!(snapshot(&probe.unrelated), probe.before);
}

fn run(root: &Path, args: &[&str]) -> anyhow::Result<()> {
    let parser = CoreCommand::augment_subcommands(clap::Command::new("fixture"));
    let matches = parser.try_get_matches_from(args)?;
    let command = CoreCommand::from_arg_matches(&matches)?;
    let ctx = Ctx::load_from_manifest(&root.join("Cargo.toml"))?;
    kithara_devtools::run(&command, &ctx)
}

#[test]
fn direction_checks_the_selected_resolved_dependency_graph() {
    let directory = tempdir().expect("dependency graph fixture");
    let root = directory.path();
    workspace(root, "portable-foundation", "");
    for (path, name, dependencies) in [
        (
            "modules/renamed-bridge",
            "portable-bridge",
            "[dependencies]\nproduct={package='portable-product',path='../../packages/renamed-product'}\n",
        ),
        ("packages/renamed-product", "portable-product", ""),
    ] {
        let package = root.join(path);
        fs::create_dir_all(package.join("src")).expect("member source directory");
        fs::write(
            package.join("Cargo.toml"),
            format!("[package]\nname='{name}'\nversion='0.1.0'\nedition='2024'\n{dependencies}"),
        )
        .expect("member manifest");
        fs::write(package.join("src/lib.rs"), "").expect("member source");
    }
    fs::create_dir(root.join(".config/arch")).expect("architecture config directory");
    fs::write(
        root.join(".config/arch/direction.toml"),
        "[[layer]]\nindex=0\nname='foundation'\ncrates=['portable-foundation']\n\n\
         [[layer]]\nindex=1\nname='product'\ncrates=['portable-product']\n",
    )
    .expect("direction policy");
    let manifest = "[package]\nname='portable-foundation'\nversion='0.1.0'\nedition='2024'\n\n\
                    [workspace]\nmembers=['modules/renamed-bridge','packages/renamed-product']\n";
    let args = [
        "fixture",
        "lint",
        "arch",
        "--check",
        "direction",
        "--crate",
        "portable-foundation",
        "--report",
        "direction.md",
    ];
    for (dependencies, forbidden) in [
        (
            "[dependencies]\nproduct={package='portable-product',path='packages/renamed-product'}\n",
            true,
        ),
        (
            "[dependencies]\nbridge={package='portable-bridge',path='modules/renamed-bridge'}\n",
            true,
        ),
        (
            "[dev-dependencies]\nproduct={package='portable-product',path='packages/renamed-product'}\n",
            false,
        ),
        ("", false),
    ] {
        fs::write(
            root.join("Cargo.toml"),
            format!("{manifest}\n{dependencies}"),
        )
        .expect("selected dependency scenario");
        let result = run(root, &args);
        if forbidden {
            result.expect_err("normal dependencies must not reach a higher layer");
        } else {
            result.expect("development-only and absent dependencies are not normal edges");
        }
        let report = fs::read_to_string(root.join("direction.md")).expect("direction report");
        assert_eq!(
            report.contains("portable-foundation -> portable-product"),
            forbidden,
            "{report}"
        );
    }
}

#[test]
fn manifest_aliases_use_semantic_target_identity() {
    let directory = tempdir().expect("target dependency fixture");
    let root = directory.path();
    workspace(root, "portable-order", "");
    for (path, name, workspace) in [
        ("modules/internal", "portable-engine", ""),
        ("vendor/a-external", "a-external", "[workspace]\n"),
    ] {
        let package = root.join(path);
        fs::create_dir_all(package.join("src")).expect("dependency source directory");
        fs::write(
            package.join("Cargo.toml"),
            format!("[package]\nname='{name}'\nversion='0.1.0'\nedition='2024'\n{workspace}"),
        )
        .expect("dependency manifest");
        fs::write(package.join("src/lib.rs"), "").expect("dependency source");
    }
    let manifest = "[package]\nname='portable-order'\nversion='0.1.0'\nedition='2024'\n\n\
                    [workspace]\nmembers=['modules/internal']\nexclude=['vendor/a-external']\n";
    let internal = "z-engine={package='portable-engine',path='modules/internal'}\n";
    let external = "a-external={path='vendor/a-external'}\n";
    let args = ["fixture", "manifest", "dependency-order"];
    for target in ["cfg(unix)", "cfg( unix )", "cfg(any(unix,windows))"] {
        let header = format!("{manifest}\n[target.'{target}'.dependencies]\n");
        fs::write(
            root.join("Cargo.toml"),
            format!("{header}{internal}\n{external}"),
        )
        .expect("ordered target dependencies");
        run(root, &args).expect("workspace alias precedes the external dependency");
        fs::write(
            root.join("Cargo.toml"),
            format!("{header}{external}\n{internal}"),
        )
        .expect("misordered target dependencies");
        run(root, &args).expect_err("target formatting must not change dependency ownership");
    }
}

#[test]
#[ignore = "fixture adapter invoked only by the asserted portable command tests"]
fn portable_command_child() {
    let root = PathBuf::from(std::env::var_os("PORTABLE_ROOT").expect("fixture target"));
    let args: Vec<String> =
        serde_json::from_str(&std::env::var("PORTABLE_ARGS").expect("fixture command"))
            .expect("decode fixture args");
    let parser = CoreCommand::augment_subcommands(clap::Command::new("fixture"));
    let matches = parser
        .try_get_matches_from(args)
        .expect("parse public command");
    let command = CoreCommand::from_arg_matches(&matches).expect("construct public command");
    let result = Ctx::load_from_manifest(&root.join("Cargo.toml"))
        .and_then(|ctx| kithara_devtools::run(&command, &ctx));
    let result = match result {
        Ok(()) => json!({"ok": true}),
        Err(error) => json!({
            "ok": false,
            "error": format!("{error:#}"),
            "exit_code": NotClean::report(&error),
        }),
    };
    fs::write(
        std::env::var_os("PORTABLE_RESULT").expect("fixture result path"),
        result.to_string(),
    )
    .expect("record public adapter result");
}
