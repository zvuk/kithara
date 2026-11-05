//! Cargo reruns a build script when a path or variable it named changes, and
//! a script that names none is rerun whenever anything in its package is
//! newer than its last run. On a build directory checkouts share, that is
//! every claim. So every workspace build script names what it reads before it
//! can return, and none of them reads a value that changes on every commit.

use std::{fs, path::Path};

use syn::{Item, Stmt, visit::Visit};

/// Values that differ on every commit or pipeline: a script that reads one
/// reruns, and rebuilds everything that depends on it, on every job.
const PER_COMMIT: &[&str] = &[
    "CI_COMMIT_SHA",
    "GITHUB_SHA",
    "CI_PIPELINE_ID",
    "GITHUB_RUN_ID",
    "--git-path",
];

#[test]
fn every_build_script_tells_cargo_what_it_reads_before_it_can_return() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask has a workspace root");
    let metadata = cargo_metadata::MetadataCommand::new()
        .manifest_path(root.join("Cargo.toml"))
        .no_deps()
        .exec()
        .expect("cargo metadata describes the workspace");
    let mut problems = Vec::new();
    let mut checked = 0;
    for package in &metadata.packages {
        for target in package
            .targets
            .iter()
            .filter(|target| target.is_custom_build())
        {
            let path = target.src_path.as_std_path();
            let source = fs::read_to_string(path).expect("a build script is readable");
            for value in PER_COMMIT {
                if source.contains(value) {
                    problems.push(format!(
                        "{}: reads {value}, which changes on every commit",
                        path.display()
                    ));
                }
            }
            let file = syn::parse_file(&source).expect("a build script parses");
            for item in &file.items {
                if let Item::Fn(main) = item
                    && main.sig.ident == "main"
                    && let Some(problem) = main_problem(&main.block.stmts)
                {
                    problems.push(format!("{}: {problem}", path.display()));
                }
            }
            checked += 1;
        }
    }
    assert!(checked > 0, "the workspace has build scripts");
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

/// Every recipe gets the pinned `ffmpeg` keg on `PKG_CONFIG_PATH` from the root
/// `justfile`, and a recipe that runs `just` again evaluates that export again.
/// A build script that reads the variable reruns when its value changes, and
/// everything built on it is rebuilt, so the value must not depend on how
/// deeply the recipe that started Cargo was nested.
#[cfg(unix)]
#[test]
fn a_nested_just_hands_build_scripts_the_same_pkg_config_path() {
    use std::{env, os::unix::fs::PermissionsExt, process::Command};

    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask has a workspace root");
    let pins =
        fs::read_to_string(root.join(".config/ci-pins.toml")).expect("the pins are readable");
    let start = pins
        .find("ffmpeg@")
        .expect("the pins name an FFmpeg formula");
    let formula: String = pins[start..]
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '@')
        .collect();
    let machine = tempfile::tempdir().expect("a temporary machine prefix");
    let brew = machine.path().join("bin/brew");
    fs::create_dir_all(machine.path().join("bin")).expect("a bin directory");
    fs::write(&brew, "#!/bin/sh\n").expect("a package manager stand-in");
    fs::set_permissions(&brew, fs::Permissions::from_mode(0o755)).expect("an executable stand-in");
    fs::create_dir_all(
        machine
            .path()
            .join("opt")
            .join(&formula)
            .join("lib/pkgconfig"),
    )
    .expect("the keg the pins name");
    let path = format!(
        "{}:{}",
        machine.path().join("bin").display(),
        env::var("PATH").unwrap_or_default()
    );
    let evaluate = |inherited: &str| {
        let output = Command::new("just")
            .arg("--justfile")
            .arg(root.join("justfile"))
            .arg("--working-directory")
            .arg(root)
            .args(["--evaluate", "PKG_CONFIG_PATH"])
            .env("PATH", &path)
            .env("PKG_CONFIG_PATH", inherited)
            .output()
            .expect("just runs");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).expect("the value is UTF-8")
    };

    let own = "/elsewhere/lib/pkgconfig";
    let outer = evaluate(own);
    assert!(outer.contains(&formula), "the keg is on the path: {outer}");
    assert!(outer.ends_with(own), "the caller's own path stays: {outer}");
    assert_eq!(evaluate(&outer), outer, "a nested just changed the value");
}

/// The push gate's arguments, then the opposite choice on every axis: browser,
/// suite and audio route all differ, so a run placed under any one of those
/// conditions is missing from one of the two.
#[cfg(unix)]
const WASM_RECIPE_RUNS: [[&str; 5]; 2] = [
    ["test", "wasm", "chrome", "webcodecs", "include"],
    ["test", "wasm", "firefox", "all", "exclude"],
];

/// One logged step of a browser recipe run.
#[cfg(unix)]
#[derive(Debug, PartialEq)]
enum WasmRecipeStep {
    /// A Cargo invocation, without its `+toolchain`.
    Cargo(Vec<String>),
    /// A start of the prebuilt native runner.
    Runner,
}

#[cfg(unix)]
struct WasmRecipeRun {
    output: std::process::Output,
    steps: Vec<WasmRecipeStep>,
}

#[cfg(unix)]
impl WasmRecipeRun {
    /// The arguments of every Cargo invocation whose command is `command`.
    fn cargo(&self, command: &str) -> impl Iterator<Item = &[String]> {
        self.steps.iter().filter_map(move |step| match step {
            WasmRecipeStep::Cargo(args) if args.first().is_some_and(|first| first == command) => {
                Some(args.as_slice())
            }
            _ => None,
        })
    }

    fn stderr(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(&self.output.stderr)
    }
}

/// Packages for the stand-in's `metadata`: one entry turns default features
/// off and names two features, one keeps the defaults and names none, and one
/// package declares nothing.
#[cfg(unix)]
fn wasm_lib_test_packages() -> serde_json::Value {
    serde_json::json!([
        {
            "name": "declares-two-features",
            "metadata": { "kithara": { "wasm-lib-tests": {
                "default-features": false,
                "features": ["first", "second"],
            } } },
        },
        {
            "name": "declares-defaults",
            "metadata": { "kithara": { "wasm-lib-tests": {
                "default-features": true,
                "features": [],
            } } },
        },
        { "name": "declares-nothing", "metadata": null },
    ])
}

/// Runs the browser recipe with `args` against a Cargo stand-in whose
/// `metadata` answers with `packages`. The stand-in logs every invocation
/// before it acts: `build` writes the native runner and its fixture server,
/// and `test` refuses any runner but the prebuilt one, then starts it.
#[cfg(unix)]
fn run_wasm_recipe(packages: &serde_json::Value, args: &[&str]) -> WasmRecipeRun {
    use std::{env, os::unix::fs::PermissionsExt, process::Command};

    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask has a workspace root");
    let fixture = tempfile::tempdir().expect("a temporary runner fixture");
    let bin = fixture.path().join("bin");
    let target = fixture.path().join("native target");
    let log = fixture.path().join("steps");
    fs::create_dir(&bin).expect("a stand-in binary directory");
    let cargo = bin.join("cargo");
    fs::write(
        &cargo,
        r#"#!/usr/bin/env python3
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys

args = sys.argv[1:]
if args[0].startswith("+"):
    args = args[1:]
with open(os.environ["WASM_RECIPE_LOG"], "a") as log:
    log.write(json.dumps(args) + "\n")
target = Path(os.environ["CARGO_TARGET_DIR"])
if args[0] == "build":
    output = target / "debug"
    output.mkdir(parents=True)
    for index, arg in enumerate(args):
        if arg == "--bin":
            binary = output / args[index + 1]
            binary.write_text('#!/bin/sh\nprintf "runner\\n" >> "$WASM_RECIPE_LOG"\n')
            binary.chmod(0o755)
elif args[0] == "metadata":
    packages = json.loads(os.environ["WASM_RECIPE_PACKAGES"])
    print(json.dumps({"target_directory": str(target), "packages": packages}))
else:
    assert args[0] == "test", args
    runner = os.environ.get("CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER", "")
    resolved = shutil.which(runner)
    assert resolved == str(target / "debug/wasm_test_runner"), "runner must be prebuilt"
    assert (target / "debug/test_server").is_file(), "fixture server must be beside runner"
    subprocess.run([resolved], check=True)
"#,
    )
    .expect("a Cargo stand-in");
    fs::set_permissions(&cargo, fs::Permissions::from_mode(0o755))
        .expect("an executable Cargo stand-in");
    let path = env::join_paths(
        std::iter::once(bin).chain(env::split_paths(&env::var_os("PATH").unwrap_or_default())),
    )
    .expect("a binary search path");
    let output = Command::new("just")
        .arg("--justfile")
        .arg(root.join("justfile"))
        .args(args)
        .env("PATH", path)
        .env("CARGO_TARGET_DIR", target)
        .env("WASM_RECIPE_LOG", &log)
        .env("WASM_RECIPE_PACKAGES", packages.to_string())
        .env_remove("CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER")
        .output()
        .expect("the browser recipe runs");
    let steps = fs::read_to_string(&log)
        .unwrap_or_default()
        .lines()
        .map(|line| {
            if line == "runner" {
                WasmRecipeStep::Runner
            } else {
                WasmRecipeStep::Cargo(
                    serde_json::from_str(line).expect("a logged Cargo invocation"),
                )
            }
        })
        .collect();
    WasmRecipeRun { output, steps }
}

/// The values that follow every occurrence of `flag` in `args`.
#[cfg(unix)]
fn flag_values<'a>(args: &'a [String], flag: &'a str) -> impl Iterator<Item = &'a str> {
    args.iter()
        .zip(args.iter().skip(1))
        .filter(move |(name, _)| *name == flag)
        .map(|(_, value)| value.as_str())
}

#[cfg(unix)]
#[test]
fn wasm_tests_run_the_prebuilt_native_runner() {
    for args in WASM_RECIPE_RUNS {
        let run = run_wasm_recipe(&wasm_lib_test_packages(), &args);
        assert!(run.output.status.success(), "{args:?}: {}", run.stderr());
        let mut tests = 0;
        for (index, step) in run.steps.iter().enumerate() {
            if let WasmRecipeStep::Cargo(cargo) = step
                && cargo.first().is_some_and(|command| command == "test")
            {
                tests += 1;
                assert_eq!(
                    run.steps.get(index + 1),
                    Some(&WasmRecipeStep::Runner),
                    "{args:?}: `cargo {}` did not start the prebuilt runner",
                    cargo.join(" ")
                );
            }
        }
        assert!(tests > 0, "{args:?}: the recipe ran no browser tests");
    }
}

#[cfg(unix)]
#[test]
fn wasm_lib_runs_follow_package_metadata() {
    let packages = wasm_lib_test_packages();
    for args in WASM_RECIPE_RUNS {
        let run = run_wasm_recipe(&packages, &args);
        assert!(run.output.status.success(), "{args:?}: {}", run.stderr());
        let mut lib_runs = std::collections::BTreeMap::<&str, Vec<&[String]>>::new();
        for test in run
            .cargo("test")
            .filter(|test| test.iter().any(|arg| arg == "--lib"))
        {
            for package in flag_values(test, "-p") {
                lib_runs.entry(package).or_default().push(test);
            }
        }
        for package in packages.as_array().expect("a package list") {
            let entry = &package["metadata"]["kithara"]["wasm-lib-tests"];
            if entry.is_null() {
                continue;
            }
            let name = package["name"].as_str().expect("a package name");
            let runs = lib_runs.remove(name).unwrap_or_default();
            let [test] = runs.as_slice() else {
                panic!("{args:?}: {name} needs exactly one --lib run, got {runs:?}");
            };
            assert_eq!(
                test.iter().any(|arg| arg == "--no-default-features"),
                entry["default-features"] == false,
                "{args:?}: {name} --no-default-features in {test:?}"
            );
            let declared: Vec<&str> = entry["features"]
                .as_array()
                .expect("a feature list")
                .iter()
                .map(|feature| feature.as_str().expect("a feature name"))
                .collect();
            let passed: Vec<&str> = flag_values(test, "--features")
                .flat_map(|list| list.split(','))
                .collect();
            assert_eq!(passed, declared, "{args:?}: {name} --features in {test:?}");
        }
        assert!(
            lib_runs.is_empty(),
            "{args:?}: packages without an entry got --lib runs: {lib_runs:?}"
        );
    }

    let omitted = serde_json::json!([{
        "name": "omits-features",
        "metadata": { "kithara": { "wasm-lib-tests": { "default-features": true } } },
    }]);
    let [push_gate, _] = WASM_RECIPE_RUNS;
    let run = run_wasm_recipe(&omitted, &push_gate);
    assert!(
        !run.output.status.success(),
        "an entry without features must stop the recipe"
    );
    let started: Vec<_> = run.cargo("build").chain(run.cargo("test")).collect();
    assert!(
        started.is_empty(),
        "the recipe built or ran before it refused the entry: {started:?}"
    );
    assert!(
        run.stderr().contains("omits-features"),
        "the refusal names the package: {}",
        run.stderr()
    );
}

/// What is wrong with a `main` that can return, or ends, before it names a
/// path or a variable to cargo.
fn main_problem(stmts: &[Stmt]) -> Option<&'static str> {
    for stmt in stmts {
        if is_directive(stmt) {
            return None;
        }
        let mut exit = Exit::default();
        exit.visit_stmt(stmt);
        if exit.found {
            return Some("returns before printing a rerun-if directive");
        }
    }
    Some("prints no rerun-if directive")
}

/// A `println!` whose format string starts with a rerun-if directive.
fn is_directive(stmt: &Stmt) -> bool {
    let Stmt::Macro(stmt) = stmt else {
        return false;
    };
    stmt.mac.path.is_ident("println")
        && stmt
            .mac
            .tokens
            .clone()
            .into_iter()
            .next()
            .is_some_and(|token| {
                let text = token.to_string();
                text.starts_with("\"cargo::rerun-if") || text.starts_with("\"cargo:rerun-if")
            })
}

/// Whether a statement can leave `main`: a `return` or a `?` outside any
/// closure or nested item.
#[derive(Default)]
struct Exit {
    found: bool,
}

impl<'ast> Visit<'ast> for Exit {
    fn visit_expr_return(&mut self, _: &'ast syn::ExprReturn) {
        self.found = true;
    }

    fn visit_expr_try(&mut self, _: &'ast syn::ExprTry) {
        self.found = true;
    }

    /// A closure's `return` leaves the closure, not `main`.
    fn visit_expr_closure(&mut self, _: &'ast syn::ExprClosure) {}

    /// A nested item's `return` leaves that item.
    fn visit_item(&mut self, _: &'ast Item) {}
}
