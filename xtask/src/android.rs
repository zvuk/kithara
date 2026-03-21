use std::{fs, path::Path, process::Command};

use anyhow::{Context, Result, bail};
use cargo_metadata::MetadataCommand;

use crate::util::{check_rust_target, check_tool};

#[derive(Clone, Copy, Debug, clap::Subcommand)]
pub(crate) enum AndroidCommand {
    /// Build Android shared libraries and Kotlin bindings.
    Build {
        /// Build profile.
        #[arg(long, default_value_t = crate::BuildProfile::Release)]
        profile: crate::BuildProfile,
    },
}

pub(crate) fn run(cmd: AndroidCommand) -> Result<()> {
    match cmd {
        AndroidCommand::Build { profile } => run_build(profile),
    }
}

const ANDROID_API_LEVEL: &str = "26";
const RUST_TARGETS: &[(&str, &str)] = &[
    ("aarch64-linux-android", "arm64-v8a"),
    ("armv7-linux-androideabi", "armeabi-v7a"),
    ("x86_64-linux-android", "x86_64"),
];

fn recreate_dir(path: &Path) -> Result<()> {
    if path.exists() {
        fs::remove_dir_all(path).with_context(|| format!("remove {}", path.display()))?;
    }
    fs::create_dir_all(path).with_context(|| format!("create_dir_all {}", path.display()))?;
    Ok(())
}

fn run_build(profile: crate::BuildProfile) -> Result<()> {
    // 1. Check prerequisites.
    check_tool("cargo", &["ndk", "--help"], "cargo install cargo-ndk")?;
    check_tool("rustup", &["--version"], "https://rustup.rs")?;

    for (target, _) in RUST_TARGETS {
        if !check_rust_target(target)? {
            bail!("Rust target '{target}' is not installed. Run: rustup target add {target}");
        }
    }

    // 2. Resolve workspace paths.
    let metadata = MetadataCommand::new()
        .exec()
        .context("failed to read cargo metadata")?;
    let root = metadata.workspace_root.as_std_path();
    let crate_dir = root.join("crates/kithara-ffi");
    let jni_dir = root.join("android/lib/build/generated/jniLibs");
    let kotlin_dir = root.join("android/lib/build/generated/uniffi/kotlin");

    // 3. Recreate output directories.
    recreate_dir(&jni_dir)?;
    recreate_dir(&kotlin_dir)?;

    // 4. Build Android shared libraries via cargo-ndk.
    println!("==> Building Android shared libraries");

    let ndk_targets: Vec<&str> = RUST_TARGETS
        .iter()
        .flat_map(|(_, abi)| ["-t", abi])
        .collect();

    let mut cmd = Command::new("cargo");
    cmd.arg("ndk")
        .arg("-P")
        .arg(ANDROID_API_LEVEL)
        .args(&ndk_targets)
        .arg("-o")
        .arg(&jni_dir)
        .args(["build", "-p", "kithara-ffi", "--features", "backend-uniffi"]);
    if matches!(profile, crate::BuildProfile::Release) {
        cmd.arg("--release");
    }
    cmd.current_dir(root);

    let status = cmd.status().context("failed to run cargo ndk")?;
    if !status.success() {
        bail!("cargo ndk failed");
    }

    // 5. Verify the arm64 library was produced.
    let lib_path = jni_dir.join("arm64-v8a/libkithara_ffi.so");
    if !lib_path.exists() {
        bail!("compiled library not found at {}", lib_path.display());
    }

    // 6. Generate Kotlin bindings via uniffi-bindgen.
    println!("==> Generating Kotlin bindings");

    let mut cmd = Command::new("cargo");
    cmd.args([
        "run",
        "--bin",
        "uniffi-bindgen",
        "--features",
        "backend-uniffi",
    ]);
    if matches!(profile, crate::BuildProfile::Release) {
        cmd.arg("--release");
    }
    cmd.args([
        "--",
        "generate",
        "--library",
        lib_path.to_str().context("lib path is not valid UTF-8")?,
        "--language",
        "kotlin",
        "--no-format",
        "--out-dir",
        kotlin_dir
            .to_str()
            .context("kotlin dir is not valid UTF-8")?,
    ]);
    cmd.current_dir(&crate_dir);

    let status = cmd.status().context("failed to run uniffi-bindgen")?;
    if !status.success() {
        bail!("uniffi-bindgen failed");
    }

    // 7. Print summary.
    println!("==> Done!");
    println!("==> JNI libs: {}", jni_dir.display());
    println!("==> Kotlin bindings: {}", kotlin_dir.display());

    Ok(())
}
