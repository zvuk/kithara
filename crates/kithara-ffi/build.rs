use std::{env, process::Command};

fn git_output(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()
        .map(|value| value.trim().to_owned())
}

fn main() {
    println!("cargo:rerun-if-env-changed=CI_COMMIT_SHA");
    println!("cargo:rerun-if-env-changed=GITHUB_SHA");

    let ci_revision = env::var("CI_COMMIT_SHA")
        .or_else(|_| env::var("GITHUB_SHA"))
        .ok();
    let revision = ci_revision.as_deref().unwrap_or("HEAD");

    let git_hash =
        git_output(&["rev-parse", "--short=8", revision]).unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=BUILD_GIT_HASH={git_hash}");

    let timestamp = git_output(&[
        "show",
        "-s",
        "--format=%cd",
        "--date=format:%m%d-%H%M%S",
        revision,
    ])
    .unwrap_or_else(|| "0000-0000".into());
    println!("cargo:rustc-env=BUILD_TIMESTAMP={timestamp}");

    // CI checkout changes HEAD's mtime even when the revision is unchanged.
    if ci_revision.is_none()
        && let Some(path) = git_output(&["rev-parse", "--git-path", "HEAD"])
    {
        println!("cargo:rerun-if-changed={path}");
    }
    println!("cargo:rerun-if-changed=src/");
    println!("cargo:rerun-if-changed=build.rs");
}
