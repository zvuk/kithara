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

#[cfg(unix)]
#[test]
fn wasm_tests_run_the_prebuilt_native_runner() {
    use std::{env, os::unix::fs::PermissionsExt, process::Command};

    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask has a workspace root");
    let fixture = tempfile::tempdir().expect("a temporary runner fixture");
    let bin = fixture.path().join("bin");
    let target = fixture.path().join("native target");
    let runs = fixture.path().join("runs");
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
target = Path(os.environ["CARGO_TARGET_DIR"])
if args[0] == "build":
    output = target / "debug"
    output.mkdir(parents=True)
    for index, arg in enumerate(args):
        if arg == "--bin":
            binary = output / args[index + 1]
            binary.write_text('#!/bin/sh\nprintf "run\\n" >> "$WASM_RECIPE_RUNS"\n')
            binary.chmod(0o755)
elif args[0] == "metadata":
    print(json.dumps({"target_directory": str(target)}))
else:
    assert "test" in args, args
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
        .args(["test", "wasm", "chrome", "webcodecs", "include"])
        .env("PATH", path)
        .env("CARGO_TARGET_DIR", target)
        .env("WASM_RECIPE_RUNS", &runs)
        .env_remove("CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER")
        .output()
        .expect("the browser recipe runs");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(runs)
            .expect("runner invocations are recorded")
            .lines()
            .count(),
        10,
        "every browser test command uses the native runner"
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
