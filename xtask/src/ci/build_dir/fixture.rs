//! Checkouts the build-directory tests claim.

use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

use tempfile::TempDir;

/// Runs git in `dir` against that repository alone: ambient `GIT_*`
/// variables of a hook would point it at the repository running the test.
pub(crate) fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_WORK_TREE")
        .args(args)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

/// A git checkout tracking `files`.
pub(crate) fn git_checkout(files: &[(&str, &str)]) -> TempDir {
    let checkout = tempfile::tempdir().unwrap();
    git(checkout.path(), &["init", "-q"]);
    for (path, content) in files {
        let file = checkout.path().join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, content).unwrap();
    }
    git(checkout.path(), &["add", "-A"]);
    checkout
}

/// Builds `checkout` with the real Cargo into `target`, reporting each unit
/// as a JSON message on stdout.
///
/// Empty wrapper variables override any `build.rustc-wrapper` or
/// `build.rustc-workspace-wrapper` from Cargo configuration, so no host
/// compiler cache takes part in the build.
pub(crate) fn cargo_build(checkout: &Path, target: &Path) -> Output {
    Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
        .current_dir(checkout)
        .env("CARGO_TARGET_DIR", target)
        .env("RUSTC_WRAPPER", "")
        .env("RUSTC_WORKSPACE_WRAPPER", "")
        .args(["build", "--offline", "--quiet", "--message-format=json"])
        .output()
        .unwrap()
}

#[test]
fn a_cargo_build_runs_no_configured_compiler_wrapper() {
    let checkout = git_checkout(&[
        (
            "Cargo.toml",
            "[package]\nname='wrapper-probe'\nversion='0.1.0'\nedition='2024'\n",
        ),
        ("src/lib.rs", ""),
        (
            ".cargo/config.toml",
            "[build]\nrustc-wrapper = 'no-compiler-wrapper'\nrustc-workspace-wrapper = 'no-compiler-wrapper'\n",
        ),
    ]);
    let target = tempfile::tempdir().unwrap();

    let output = cargo_build(checkout.path(), target.path());

    assert!(output.status.success(), "{output:?}");
}
