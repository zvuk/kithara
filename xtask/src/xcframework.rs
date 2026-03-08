use std::{
    fs,
    path::Path,
    process::{Command, Stdio},
};

use anyhow::{Context, Result, bail};
use cargo_metadata::MetadataCommand;

/// Recursively copy `src` directory to `dst`.
fn copy_dir_all(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src).with_context(|| format!("read_dir {}", src.display()))? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            copy_dir_all(&src_path, &dst_path)?;
        } else {
            fs::copy(&src_path, &dst_path).with_context(|| {
                format!("copy {} -> {}", src_path.display(), dst_path.display())
            })?;
        }
    }
    Ok(())
}

pub(crate) fn run(profile: crate::BuildProfile) -> Result<()> {
    // 1. Check cargo-swift is installed.
    match Command::new("cargo")
        .args(["swift", "--help"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        Ok(s) if s.success() => {}
        _ => bail!("cargo-swift not found. Install with: cargo install cargo-swift"),
    }

    // 2. Resolve workspace paths.
    let metadata = MetadataCommand::new()
        .exec()
        .context("failed to read cargo metadata")?;
    let root = metadata.workspace_root.as_std_path();
    let crate_dir = root.join("crates/kithara-ffi");
    let apple_dir = root.join("apple");

    // 3. Run cargo swift package.
    println!("==> Building KitharaFFI with cargo-swift");

    let mut cmd = Command::new("cargo");
    cmd.args(["swift", "package", "-p", "ios", "macos", "-n", "KitharaFFI"]);
    if matches!(profile, crate::BuildProfile::Release) {
        cmd.arg("--release");
    }
    cmd.args([
        "--lib-type",
        "static",
        "-F",
        "backend-uniffi",
        "--swift-tools-version",
        "6.0",
        "-y",
    ]);
    cmd.current_dir(&crate_dir);

    let status = cmd.status().context("failed to run cargo swift package")?;
    if !status.success() {
        bail!("cargo swift package failed");
    }

    // 4. Copy outputs to apple/.
    println!("==> Copying outputs to apple/");

    let xcf_src = crate_dir.join("KitharaFFI/KitharaFFIInternal.xcframework");
    let xcf_dst = apple_dir.join("KitharaFFIInternal.xcframework");
    let swift_src = crate_dir.join("KitharaFFI/Sources/KitharaFFI/KitharaFFI.swift");
    let swift_dst = apple_dir.join("Sources/KitharaFFI/KitharaFFI.swift");

    // Remove old xcframework if present, then copy recursively.
    if xcf_dst.exists() {
        fs::remove_dir_all(&xcf_dst).with_context(|| format!("remove {}", xcf_dst.display()))?;
    }
    copy_dir_all(&xcf_src, &xcf_dst)
        .with_context(|| format!("copy {} -> {}", xcf_src.display(), xcf_dst.display()))?;

    // Copy generated Swift bindings.
    if let Some(parent) = swift_dst.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(&swift_src, &swift_dst)
        .with_context(|| format!("copy {} -> {}", swift_src.display(), swift_dst.display()))?;

    // 5. Print summary.
    println!("==> Done!");
    println!("==> XCFramework: {}", xcf_dst.display());
    println!("==> Swift bindings: {}", swift_dst.display());

    println!();
    println!("XCFramework slices:");
    if let Ok(entries) = fs::read_dir(&xcf_dst) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                println!("  {}/", path.display());
            }
        }
    }

    println!();
    println!("To build and test:");
    println!("  cd {} && swift build && swift test", apple_dir.display());

    Ok(())
}
