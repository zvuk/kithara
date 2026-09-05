use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use cargo_metadata::MetadataCommand;
use kithara_devtools::{Ctx, common::tools::ToolsConfig};
use plist::Value as PlistValue;
use regex::Regex;

use crate::{
    apple_docgen,
    config::{AppleConfig, KitharaExt, ReleaseConfig},
};

/// Module constants for Apple build protocol details. Grouped per the
/// `style.multiple-private-module-consts` lint.
struct Consts;
impl Consts {
    /// `panic=immediate-abort` lowers every panic to a trap, so the
    /// `core::fmt` panic plumbing drops out of each slice; same lane as the
    /// wasm flags in `crates/kithara-ffi/.cargo/config.toml`.
    ///
    /// No `embed-bitcode=no` here: `relink_slices_with_lto` runs fat LTO over
    /// the slice, and LTO consumes exactly the rlib bitcode that flag
    /// suppresses. rustc rejects the two together for the same reason.
    const RELEASE_RUSTFLAGS: &[&str] = &["-Z", "unstable-options", "-C", "panic=immediate-abort"];
    /// Feature set of every release slice, shared by the `cargo swift`
    /// packaging build and the whole-program relink so both see one graph.
    const SLICE_FEATURES: &str = "uniffi,apple,dev,stretch-signalsmith";
    /// Target triples behind each `*.xcframework` slice directory. Universal
    /// slices list every arch and are recombined with `lipo -create`.
    const SLICE_TARGETS: &[(&str, &[&str])] = &[
        ("ios-arm64", &["aarch64-apple-ios"]),
        (Self::IOS_SIMULATOR_SLICE, &["aarch64-apple-ios-sim"]),
        (
            Self::IOS_SIMULATOR_FAT_SLICE,
            &["aarch64-apple-ios-sim", "x86_64-apple-ios"],
        ),
        (
            "macos-arm64_x86_64",
            &["aarch64-apple-darwin", "x86_64-apple-darwin"],
        ),
    ];
    /// `+nightly` propagates to the nested `cargo build` processes that
    /// cargo-swift spawns (rustup exports `RUSTUP_TOOLCHAIN`), which is what
    /// activates the `[unstable] build-std` section of
    /// `crates/kithara-ffi/.cargo/config.toml` for every slice. `-Z` CLI
    /// flags must not be added here: the outer cargo consumes them without
    /// forwarding to external subcommands, so they silently do nothing.
    const RELEASE_CARGO_ARGS: &[&str] = &["+nightly"];
    /// Slice subdirectories inside the `*.xcframework` we expect to find.
    const XCFRAMEWORK_SLICES: &[&str] =
        &["ios-arm64", Self::IOS_SIMULATOR_SLICE, "macos-arm64_x86_64"];
    const IOS_SIMULATOR_FAT_SLICE: &'static str = "ios-arm64_x86_64-simulator";
    const IOS_SIMULATOR_SLICE: &'static str = "ios-arm64-simulator";
}

/// Project-agnostic single-framework packaging config, read from
/// `[workspace.metadata.apple]`. Nothing here is hard-coded in the build
/// logic, so the `apple single` tooling lifts into any `UniFFI` + Swift
/// workspace by editing that metadata table.
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
struct SingleFrameworkSpec {
    /// Swift module + framework name (e.g. `Kithara`).
    framework_name: String,
    /// `CFBundleIdentifier` for the generated framework.
    bundle_id: String,
    /// `CFBundleShortVersionString` (must be numeric for the plist).
    short_version: String,
    /// `MinimumOSVersion` / build target (e.g. `15.6`).
    deployment_target: String,
    /// System frameworks the Rust core needs autolinked into the static
    /// framework (e.g. `AudioToolbox`, `CoreAudio`).
    autolink_frameworks: Vec<String>,
    /// `CFBundleVersion` build number; defaults to `1`.
    #[serde(default = "default_bundle_version")]
    bundle_version: String,
}

fn default_bundle_version() -> String {
    "1".to_string()
}

/// `Info.plist` keys for a single-platform `.framework`, serialized via the
/// `plist` crate (no hand-written XML).
#[derive(serde::Serialize)]
#[serde(rename_all = "PascalCase")]
struct FrameworkInfoPlist {
    #[serde(rename = "CFBundleExecutable")]
    executable: String,
    #[serde(rename = "CFBundleIdentifier")]
    identifier: String,
    #[serde(rename = "CFBundleInfoDictionaryVersion")]
    info_dictionary_version: String,
    #[serde(rename = "CFBundleName")]
    name: String,
    #[serde(rename = "CFBundlePackageType")]
    package_type: String,
    #[serde(rename = "CFBundleShortVersionString")]
    short_version: String,
    #[serde(rename = "CFBundleVersion")]
    bundle_version: String,
    #[serde(rename = "MinimumOSVersion")]
    minimum_os: String,
    #[serde(rename = "CFBundleSupportedPlatforms")]
    supported_platforms: Vec<String>,
}

/// Load the single-framework spec from `[workspace.metadata.apple]`.
fn load_spec(metadata: &cargo_metadata::Metadata) -> Result<SingleFrameworkSpec> {
    let value = metadata
        .workspace_metadata
        .get("apple")
        .context("missing [workspace.metadata.apple] table in the workspace Cargo.toml")?;
    serde_json::from_value(value.clone()).context("invalid [workspace.metadata.apple] table")
}

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

struct TempWorkDir {
    path: PathBuf,
}

impl TempWorkDir {
    fn create(prefix: &str) -> Result<Self> {
        let epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("system clock is before UNIX_EPOCH")?;
        let path = env::temp_dir().join(format!(
            "{prefix}-{}-{}",
            std::process::id(),
            epoch.as_nanos()
        ));
        fs::create_dir_all(&path).with_context(|| format!("create {}", path.display()))?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempWorkDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

struct HakariDisableGuard {
    manifest: PathBuf,
    original_manifest: String,
    lockfile: PathBuf,
    original_lockfile: String,
    active: bool,
}

impl HakariDisableGuard {
    fn disable(workspace_root: &Path) -> Result<Self> {
        let manifest = workspace_root.join("crates/kithara-workspace-hack/Cargo.toml");
        let lockfile = workspace_root.join("Cargo.lock");
        let original_manifest = fs::read_to_string(&manifest)
            .with_context(|| format!("read {}", manifest.display()))?;
        let original_lockfile = fs::read_to_string(&lockfile)
            .with_context(|| format!("read {}", lockfile.display()))?;
        let mut guard = Self {
            manifest,
            original_manifest,
            lockfile,
            original_lockfile,
            active: true,
        };

        println!("==> Temporarily disabling hakari workspace-hack for release build");
        let status = Command::new("cargo")
            .args(["hakari", "disable"])
            .current_dir(workspace_root)
            .status()
            .context("failed to run cargo hakari disable")?;
        if !status.success() {
            guard.restore().context(
                "cargo hakari disable failed, then restoring kithara-workspace-hack also failed",
            )?;
            bail!("cargo hakari disable failed");
        }

        Ok(guard)
    }

    fn restore(&mut self) -> Result<()> {
        if self.active {
            fs::write(&self.manifest, &self.original_manifest)
                .with_context(|| format!("restore {}", self.manifest.display()))?;
            fs::write(&self.lockfile, &self.original_lockfile)
                .with_context(|| format!("restore {}", self.lockfile.display()))?;
            self.active = false;
        }
        Ok(())
    }
}

impl Drop for HakariDisableGuard {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

#[derive(Clone, Debug, clap::Subcommand)]
pub(crate) enum AppleCommand {
    /// Build `XCFramework` for Apple platforms.
    Build {
        /// Build profile.
        #[arg(long, default_value_t = crate::BuildProfile::Release)]
        profile: crate::BuildProfile,
        /// Build one Rust target triple instead of all Apple platforms.
        #[arg(long)]
        target: Option<String>,
    },
    /// Build ONE self-contained `Kithara.xcframework` (Swift API + `UniFFI`
    /// binding + Rust core merged into a single module) for manual drag-in
    /// consumers. See `apple/README.md` "Distribution channels".
    Single {
        /// Build profile for the underlying Rust `XCFramework`.
        #[arg(long, default_value_t = crate::BuildProfile::Release)]
        profile: crate::BuildProfile,
    },
    /// Build the iOS demo, install on a simulator, and launch it.
    ///
    /// Pass `--debug` to launch with `simctl launch --wait-for-debugger`,
    /// which suspends the app on entry; the printed PID can then be
    /// fed to `lldb` (or Zed's `CodeLLDB` "attach" debug config).
    Run {
        /// Simulator name or UUID (defaults to a recent iPhone).
        #[arg(long)]
        simulator: Option<String>,
        /// Xcode scheme to build (e.g. `KitharaDemo_iOS`).
        #[arg(long)]
        scheme: Option<String>,
        /// Configuration: Debug or Release.
        #[arg(long, default_value_t = crate::BuildProfile::Debug)]
        profile: crate::BuildProfile,
        /// Suspend the launched app waiting for an LLDB attach.
        #[arg(long)]
        debug: bool,
        /// Skip the prerequisite `XCFramework` rebuild — assume the
        /// `apple/KitharaFFIInternal.xcframework` is already current.
        #[arg(long)]
        skip_framework: bool,
    },
    /// Audit symbols in an Apple `XCFramework`: assert that no
    /// software-fallback backend (Symphonia / fdk-aac) leaked into
    /// any slice. Used as a pre-publish gate from `apple release`.
    Audit {
        /// Path to the `*.xcframework` directory (e.g.
        /// `apple/KitharaFFIInternal.xcframework`).
        path: PathBuf,
    },
    /// Generate DocC documentation-extension pages from Rust rustdoc JSON.
    Docgen {
        /// Verify rustdoc JSON compatibility and allowlist coverage without writing files.
        #[arg(long)]
        check: bool,
    },
    /// Build release Apple artifacts, strip/audit them, zip them, and print
    /// the SPM checksum for the Rust `XCFramework` binary target.
    Release,
}

pub(crate) fn run(cmd: AppleCommand, ctx: &Ctx) -> Result<()> {
    let ext = KitharaExt::from_ctx(ctx)?;
    let tools = &ctx.config.tools;
    match cmd {
        AppleCommand::Build { profile, target } => run_build(profile, target.as_deref(), tools),
        AppleCommand::Single { profile } => run_single(profile, tools),
        AppleCommand::Run {
            simulator,
            scheme,
            profile,
            debug,
            skip_framework,
        } => run_app(
            simulator.as_deref(),
            scheme.as_deref(),
            profile,
            debug,
            skip_framework,
            &ext.apple,
            tools,
        ),
        AppleCommand::Audit { path } => audit_symbols(&path, &ext.apple, tools),
        AppleCommand::Docgen { check } => apple_docgen::run(check, &ext.apple.docgen),
        AppleCommand::Release => run_release(&ext.release, &ext.apple, tools),
    }
}

/// Run `nm` on every slice's static lib and fail if any
/// software-backend symbol survived linking or the Apple dispatcher
/// went missing.
fn audit_symbols(xcframework_dir: &Path, apple: &AppleConfig, tools: &ToolsConfig) -> Result<()> {
    if !xcframework_dir.is_dir() {
        bail!(
            "xcframework path does not exist or is not a directory: {}",
            xcframework_dir.display()
        );
    }
    let banned_symbol_needles =
        require_apple_needles(&apple.banned_symbol_needles, "banned_symbol_needles")?;
    let apple_proof_needles =
        require_apple_needles(&apple.apple_proof_needles, "apple_proof_needles")?;
    let mut errors: Vec<String> = Vec::new();
    for slice in Consts::XCFRAMEWORK_SLICES {
        let lib = xcframework_dir.join(slice).join("libkithara_ffi.a");
        if !lib.is_file() {
            errors.push(format!(
                "slice missing: {} (no libkithara_ffi.a — xcframework layout wrong?)",
                lib.display()
            ));
            continue;
        }
        let output = Command::new(symbol_tool(tools))
            .arg(&lib)
            .output()
            .with_context(|| format!("invoke symbol audit on {}", lib.display()))?;
        let symbols = String::from_utf8_lossy(&output.stdout);
        let strings = archive_strings(&lib, tools)?;
        for needle in banned_symbol_needles {
            let count =
                symbols.matches(needle.as_str()).count() + strings.matches(needle.as_str()).count();
            if count > 0 {
                errors.push(format!(
                    "slice `{slice}` leaked {count} `{needle}` symbols — \
                     software-backend dep must stay behind the symphonia feature gate"
                ));
            }
        }
        let has_apple_proof = apple_proof_needles
            .iter()
            .any(|n| symbols.contains(n.as_str()) || strings.contains(n.as_str()));
        if !has_apple_proof {
            errors.push(format!(
                "slice `{slice}` missing Apple-backend proof symbols \
                 ({apple_proof_needles:?}) — AppleCodec not linked?"
            ));
        }
    }
    if !errors.is_empty() {
        bail!(
            "Apple xcframework symbol audit failed ({} issues):\n  - {}",
            errors.len(),
            errors.join("\n  - ")
        );
    }
    println!(
        "==> Apple xcframework symbol audit passed: 0 banned symbols, AppleCodec linked in all {} slices",
        Consts::XCFRAMEWORK_SLICES.len()
    );
    Ok(())
}

/// The sysroot's `llvm-nm` when the toolchain ships one, and the configured
/// `nm` otherwise. `archive_strings` resolves `strings` through the table for
/// the same audit, so the fallback resolves too — half a configurable
/// operation sends a machine that redirected one tool to the wrong other one.
fn symbol_tool(tools: &ToolsConfig) -> PathBuf {
    rust_tool("llvm-nm").unwrap_or_else(|| PathBuf::from(tools.program("nm")))
}

fn rust_tool(name: &str) -> Option<PathBuf> {
    let output = Command::new("rustc")
        .args(["--print", "sysroot"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let sysroot = String::from_utf8(output.stdout).ok()?;
    let rustlib = PathBuf::from(sysroot.trim()).join("lib/rustlib");
    for entry in fs::read_dir(rustlib).ok()? {
        let path = entry.ok()?.path().join("bin").join(name);
        if path.is_file() {
            return Some(path);
        }
    }
    None
}

fn archive_strings(lib: &Path, tools: &ToolsConfig) -> Result<String> {
    let program = tools.program("strings");
    let output = Command::new(program)
        .arg(lib)
        .output()
        .with_context(|| format!("invoke {program} on {}", lib.display()))?;
    if !output.status.success() {
        bail!("{program} failed for {}", lib.display());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn run_build(
    profile: crate::BuildProfile,
    target: Option<&str>,
    tools: &ToolsConfig,
) -> Result<()> {
    match Command::new("cargo")
        .args(["swift", "--help"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        Ok(s) if s.success() => {}
        _ => bail!("cargo-swift not found. Install with: cargo install cargo-swift"),
    }

    let metadata = MetadataCommand::new()
        .exec()
        .context("failed to read cargo metadata")?;
    let deployment_target = load_spec(&metadata)?.deployment_target;
    let root = metadata.workspace_root.as_std_path();
    let crate_dir = root.join("crates/kithara-ffi");
    let apple_dir = root.join("apple");
    let mut hakari_guard = if matches!(profile, crate::BuildProfile::Release) {
        Some(HakariDisableGuard::disable(root)?)
    } else {
        None
    };

    println!("==> Building KitharaFFI with cargo-swift");

    let mut cmd = Command::new("cargo");
    if matches!(profile, crate::BuildProfile::Release) {
        cmd.args(Consts::RELEASE_CARGO_ARGS);
    }
    cmd.args(["swift", "package"]);
    if let Some(target) = target {
        cmd.args(["--target", target]);
    } else {
        cmd.args(["-p", "ios", "macos"]);
    }
    cmd.args(["-n", "KitharaFFI"]);
    if matches!(profile, crate::BuildProfile::Release) {
        cmd.arg("--release");
        set_release_rustflags(&mut cmd);
    }
    cmd.args([
        "--lib-type",
        "static",
        // Device build: drop default features so `symphonia` is absent —
        // the Apple AudioToolbox backend is the sole decoder on-device.
        "--no-default-features",
        "-F",
        Consts::SLICE_FEATURES,
        "--swift-tools-version",
        "6.0",
        "-y",
    ]);
    cmd.current_dir(&crate_dir);
    cmd.env("IPHONEOS_DEPLOYMENT_TARGET", &deployment_target);
    set_simulator_bindgen_args(&mut cmd, tools)?;

    let status = cmd.status().context("failed to run cargo swift package")?;
    if !status.success() {
        bail!("cargo swift package failed");
    }

    println!("==> Copying outputs to apple/");

    let xcf_src = crate_dir.join("KitharaFFI/KitharaFFIInternal.xcframework");
    let xcf_dst = apple_dir.join("KitharaFFIInternal.xcframework");
    let swift_src = crate_dir.join("KitharaFFI/Sources/KitharaFFI/KitharaFFI.swift");
    let swift_dst = apple_dir.join("Sources/KitharaFFI/KitharaFFI.swift");

    if xcf_dst.exists() {
        fs::remove_dir_all(&xcf_dst).with_context(|| format!("remove {}", xcf_dst.display()))?;
    }
    copy_dir_all(&xcf_src, &xcf_dst)
        .with_context(|| format!("copy {} -> {}", xcf_src.display(), xcf_dst.display()))?;
    if target.is_none() {
        keep_arm64_ios_simulator_only(&xcf_dst, tools)?;
    }
    if matches!(profile, crate::BuildProfile::Release) {
        relink_slices_with_lto(
            &xcf_dst,
            &crate_dir,
            metadata.target_directory.as_std_path(),
            &deployment_target,
            tools,
        )?;
        strip_xcframework(&xcf_dst, tools)?;
    }

    if let Some(parent) = swift_dst.parent() {
        fs::create_dir_all(parent)?;
    }
    let swift =
        fs::read_to_string(&swift_src).with_context(|| format!("read {}", swift_src.display()))?;
    fs::write(&swift_dst, normalize_generated_swift(&swift))
        .with_context(|| format!("write {}", swift_dst.display()))?;

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
    let program = tools.program("swift");
    println!(
        "  cd {} && {program} build && {program} test",
        apple_dir.display()
    );

    if let Some(guard) = &mut hakari_guard {
        guard.restore()?;
    }

    Ok(())
}

fn normalize_generated_swift(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    for line in src.lines() {
        out.push_str(line.trim_end_matches([' ', '\t']));
        out.push('\n');
    }
    out
}

/// `signalsmith-stretch` runs `bindgen`, which derives clang's target from
/// cargo's `TARGET`. For simulator slices that yields `arm64-apple-ios-sim`,
/// but libclang wants `-simulator`; pin valid simulator triples and sysroot.
fn set_simulator_bindgen_args(cmd: &mut Command, tools: &ToolsConfig) -> Result<()> {
    let sim_sdk = sdk_path("iphonesimulator", tools)?;
    let sim_sdk = sim_sdk
        .to_str()
        .context("iphonesimulator SDK path is not UTF-8")?;
    for (key, triple) in [
        (
            "BINDGEN_EXTRA_CLANG_ARGS_aarch64_apple_ios_sim",
            "arm64-apple-ios-simulator",
        ),
        (
            "BINDGEN_EXTRA_CLANG_ARGS_x86_64_apple_ios",
            "x86_64-apple-ios-simulator",
        ),
    ] {
        cmd.env(key, format!("--target={triple} -isysroot {sim_sdk}"));
    }
    Ok(())
}

fn set_release_rustflags(cmd: &mut Command) {
    let mut flags = env::var("CARGO_ENCODED_RUSTFLAGS").unwrap_or_default();
    for flag in Consts::RELEASE_RUSTFLAGS {
        if !flags.is_empty() {
            flags.push('\x1f');
        }
        flags.push_str(flag);
    }
    cmd.env("CARGO_ENCODED_RUSTFLAGS", flags);
}

fn run_release(release: &ReleaseConfig, apple: &AppleConfig, tools: &ToolsConfig) -> Result<()> {
    let metadata = MetadataCommand::new()
        .exec()
        .context("failed to read cargo metadata")?;
    let root = metadata.workspace_root.as_std_path().to_path_buf();
    if release.core_asset.trim().is_empty() {
        bail!("ext.release.core_asset is not set in .config/xtask.toml");
    }
    if release.merged_asset.trim().is_empty() {
        bail!("ext.release.merged_asset is not set in .config/xtask.toml");
    }

    let apple_dir = root.join("apple");
    let internal = apple_dir.join("KitharaFFIInternal.xcframework");

    run_build(crate::BuildProfile::Release, None, tools)?;
    audit_symbols(&internal, apple, tools)?;
    run_single(crate::BuildProfile::Release, tools)?;

    let tmp = env::temp_dir();
    let internal_zip = tmp.join(&release.core_asset);
    let single_zip = tmp.join(&release.merged_asset);
    zip_dir(
        &apple_dir,
        "KitharaFFIInternal.xcframework",
        &internal_zip,
        tools,
    )?;

    let single_dir = release
        .merged_asset
        .strip_suffix(".zip")
        .context("release.merged_asset must end with .zip")?;
    zip_dir(&apple_dir.join("dist"), single_dir, &single_zip, tools)?;

    let checksum = swift_checksum(&internal_zip, tools)?;
    let checksum_file = tmp.join(format!("{}.sha256", release.core_asset));
    fs::write(&checksum_file, format!("{checksum}\n"))
        .with_context(|| format!("write {}", checksum_file.display()))?;

    println!("==> Release artifacts:");
    println!("    {}", internal_zip.display());
    println!("    {}", single_zip.display());
    println!("    {}", checksum_file.display());
    println!("==> SPM checksum: {checksum}");
    Ok(())
}

/// Rebuild every packaged slice as a whole-program archive.
///
/// `cargo swift` builds `kithara-ffi` with its declared `crate-type`
/// (`lib`, `staticlib`, `cdylib`), and cargo skips LTO for any unit that also
/// emits an rlib, so the packaged archive is a pile of per-crate objects that
/// the profile's `lto = "fat"` never touched. A `staticlib`-only unit is the
/// shape fat LTO accepts; rebuild each slice that way and swap it in.
fn relink_slices_with_lto(
    xcframework: &Path,
    crate_dir: &Path,
    target_dir: &Path,
    deployment_target: &str,
    tools: &ToolsConfig,
) -> Result<()> {
    require_dir(xcframework)?;
    for entry in
        fs::read_dir(xcframework).with_context(|| format!("read {}", xcframework.display()))?
    {
        let slice_dir = entry?.path();
        let lib = slice_dir.join("libkithara_ffi.a");
        if !lib.is_file() {
            continue;
        }
        let slice = slice_dir
            .file_name()
            .and_then(|name| name.to_str())
            .with_context(|| format!("slice name is not UTF-8: {}", slice_dir.display()))?;
        let targets = Consts::SLICE_TARGETS
            .iter()
            .find_map(|(name, targets)| (*name == slice).then_some(*targets))
            .with_context(|| {
                format!("no target triples registered for xcframework slice `{slice}`")
            })?;

        println!("==> Relinking {} with fat LTO", lib.display());
        let archives = targets
            .iter()
            .map(|target| {
                build_slice_staticlib(crate_dir, target_dir, target, deployment_target, tools)
            })
            .collect::<Result<Vec<_>>>()?;
        match archives.as_slice() {
            [thin] => {
                fs::copy(thin, &lib)
                    .with_context(|| format!("copy {} -> {}", thin.display(), lib.display()))?;
            }
            fat => lipo_create(fat, &lib, tools)?,
        }
    }
    Ok(())
}

/// Build one target's `kithara-ffi` archive as a `staticlib`-only unit and
/// return its path. `cargo rustc` overrides the manifest `crate-type`, which
/// is what lets cargo turn fat LTO on for this unit.
fn build_slice_staticlib(
    crate_dir: &Path,
    target_dir: &Path,
    target: &str,
    deployment_target: &str,
    tools: &ToolsConfig,
) -> Result<PathBuf> {
    let mut cmd = Command::new("cargo");
    cmd.args(Consts::RELEASE_CARGO_ARGS);
    cmd.args([
        "rustc",
        "-p",
        "kithara-ffi",
        "--release",
        "--target",
        target,
        "--no-default-features",
        "-F",
        Consts::SLICE_FEATURES,
        "--crate-type",
        "staticlib",
    ]);
    cmd.current_dir(crate_dir);
    cmd.env("IPHONEOS_DEPLOYMENT_TARGET", deployment_target);
    set_release_rustflags(&mut cmd);
    set_simulator_bindgen_args(&mut cmd, tools)?;

    let status = cmd
        .status()
        .with_context(|| format!("failed to run cargo rustc for {target}"))?;
    if !status.success() {
        bail!("staticlib build failed for {target}");
    }
    let lib = target_dir
        .join(target)
        .join("release")
        .join("libkithara_ffi.a");
    require_file(&lib)?;
    Ok(lib)
}

fn strip_xcframework(xcframework: &Path, tools: &ToolsConfig) -> Result<()> {
    require_dir(xcframework)?;
    let program = tools.program("strip");
    for entry in
        fs::read_dir(xcframework).with_context(|| format!("read {}", xcframework.display()))?
    {
        let slice = entry?.path();
        if !slice.is_dir() {
            continue;
        }
        let lib = slice.join("libkithara_ffi.a");
        if !lib.is_file() {
            continue;
        }
        println!("==> Stripping {}", lib.display());
        let status = Command::new(program)
            .args(["-S", "-x"])
            .arg(&lib)
            .status()
            .with_context(|| format!("{program} {}", lib.display()))?;
        if !status.success() {
            bail!("{program} failed for {}", lib.display());
        }
    }
    Ok(())
}

fn keep_arm64_ios_simulator_only(xcframework: &Path, tools: &ToolsConfig) -> Result<()> {
    let fat = xcframework.join(Consts::IOS_SIMULATOR_FAT_SLICE);
    let thin = xcframework.join(Consts::IOS_SIMULATOR_SLICE);
    if fat.exists() {
        if thin.exists() {
            fs::remove_dir_all(&thin).with_context(|| format!("remove {}", thin.display()))?;
        }
        fs::rename(&fat, &thin)
            .with_context(|| format!("rename {} -> {}", fat.display(), thin.display()))?;
        let lib = thin.join("libkithara_ffi.a");
        let tmp = thin.join("libkithara_ffi.arm64.a");
        lipo_thin(&lib, "arm64", &tmp, tools)?;
        fs::rename(&tmp, &lib)
            .with_context(|| format!("replace {} with {}", lib.display(), tmp.display()))?;
    } else if !thin.exists() {
        bail!(
            "missing iOS simulator slice: expected {} or {} under {}",
            Consts::IOS_SIMULATOR_SLICE,
            Consts::IOS_SIMULATOR_FAT_SLICE,
            xcframework.display()
        );
    }
    update_ios_simulator_plist(xcframework)
}

fn update_ios_simulator_plist(xcframework: &Path) -> Result<()> {
    let plist = xcframework.join("Info.plist");
    let mut root =
        PlistValue::from_file(&plist).with_context(|| format!("read {}", plist.display()))?;
    let libraries = root
        .as_dictionary_mut()
        .and_then(|dict| dict.get_mut("AvailableLibraries"))
        .and_then(PlistValue::as_array_mut)
        .with_context(|| format!("invalid xcframework plist {}", plist.display()))?;

    let mut updated = false;
    for library in libraries {
        let Some(dict) = library.as_dictionary_mut() else {
            continue;
        };
        let platform = dict
            .get("SupportedPlatform")
            .and_then(PlistValue::as_string);
        let variant = dict
            .get("SupportedPlatformVariant")
            .and_then(PlistValue::as_string);
        if platform == Some("ios") && variant == Some("simulator") {
            dict.insert(
                "LibraryIdentifier".into(),
                PlistValue::String(Consts::IOS_SIMULATOR_SLICE.to_string()),
            );
            dict.insert(
                "SupportedArchitectures".into(),
                PlistValue::Array(vec![PlistValue::String("arm64".to_string())]),
            );
            updated = true;
        }
    }

    if !updated {
        bail!("missing iOS simulator library entry in {}", plist.display());
    }
    plist::to_file_xml(&plist, &root).with_context(|| format!("write {}", plist.display()))
}

fn zip_dir(parent: &Path, directory_name: &str, output: &Path, tools: &ToolsConfig) -> Result<()> {
    let source = parent.join(directory_name);
    require_dir(&source)?;
    if output.exists() {
        fs::remove_file(output).with_context(|| format!("remove {}", output.display()))?;
    }
    println!("==> Zipping {} -> {}", source.display(), output.display());
    let program = tools.program("zip");
    let status = Command::new(program)
        .args(["-r", "-y"])
        .arg(output)
        .arg(directory_name)
        .current_dir(parent)
        .status()
        .with_context(|| format!("{program} {}", source.display()))?;
    if !status.success() {
        bail!("{program} failed for {}", source.display());
    }
    Ok(())
}

fn swift_checksum(zip: &Path, tools: &ToolsConfig) -> Result<String> {
    let program = tools.program("swift");
    let output = Command::new(program)
        .args(["package", "compute-checksum"])
        .arg(zip)
        .output()
        .with_context(|| format!("run {program} package compute-checksum"))?;
    if !output.status.success() {
        bail!(
            "{program} package compute-checksum failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Per-arch inputs for one slice of the single-framework build.
struct ArchBuild<'a> {
    module: &'a str,
    triple: &'a str,
    sdk: &'a Path,
    module_map: &'a Path,
    rust_lib: &'a Path,
    out: &'a Path,
    module_triple: &'a str,
    rx_out: &'a Path,
}

/// Build ONE self-contained `Kithara.xcframework`.
///
/// The three Swift layers (`KitharaFFI` generated binding, `Kithara` API,
/// `KitharaRx`) are merged into a single module and the Rust static lib is
/// merged into the framework binary, so a manual drag-in consumer needs no
/// extra modules or flags. See `apple/README.md` for why the merge +
/// `internal import` post-pass is necessary (`UniFFI` leaks `RustBuffer`).
fn run_single(profile: crate::BuildProfile, tools: &ToolsConfig) -> Result<()> {
    let metadata = MetadataCommand::new()
        .exec()
        .context("failed to read cargo metadata")?;
    let spec = load_spec(&metadata)?;
    let root = metadata.workspace_root.as_std_path().to_path_buf();
    let apple_dir = root.join("apple");
    let internal = apple_dir.join("KitharaFFIInternal.xcframework");

    let built_internal = if internal.exists() {
        false
    } else {
        println!("==> KitharaFFIInternal.xcframework not found — building it first");
        run_build(profile, None, tools)?;
        true
    };
    require_dir(&internal)?;
    keep_arm64_ios_simulator_only(&internal, tools)?;
    if matches!(profile, crate::BuildProfile::Release) && !built_internal {
        strip_xcframework(&internal, tools)?;
    }

    let rx_src = resolve_rxswift(&root, tools)?;
    println!("==> RxSwift source: {}", rx_src.display());

    let temp = TempWorkDir::create(&format!(
        "{}-apple-single",
        kithara_devtools::util::project_name()
    ))?;
    let work = temp.path().to_path_buf();
    let merged = work.join("merged");
    fs::create_dir_all(&merged)?;

    println!("==> Merging the Swift layers into one module");
    merge_sources(&apple_dir, &merged, &spec.autolink_frameworks)?;

    let dist = apple_dir.join("dist");
    fs::create_dir_all(&dist)?;
    let out = dist.join(format!("{}.xcframework", spec.framework_name));
    if out.exists() {
        fs::remove_dir_all(&out).with_context(|| format!("remove {}", out.display()))?;
    }

    build_single_xcframework(&merged, &internal, &rx_src, &work, &out, &spec, tools)?;
    verify_single(&out)?;

    println!("==> Done!");
    println!("==> Single XCFramework: {}", out.display());
    Ok(())
}

/// Resolve the pinned `RxSwift` checkout via `SwiftPM` (the merged module imports
/// `RxSwift`, so its module must exist at compile time).
fn resolve_rxswift(root: &Path, tools: &ToolsConfig) -> Result<PathBuf> {
    println!("==> Resolving RxSwift via SwiftPM");
    let program = tools.program("swift");
    let status = Command::new(program)
        .args(["package", "resolve"])
        .current_dir(root)
        .env("KITHARA_LOCAL_DEV", "1")
        .status()
        .with_context(|| format!("failed to run {program} package resolve"))?;
    if !status.success() {
        bail!("{program} package resolve failed");
    }
    let rx = root.join(".build/checkouts/RxSwift/Sources/RxSwift");
    if !rx.is_dir() {
        bail!("RxSwift sources not found at {}", rx.display());
    }
    Ok(rx)
}

/// Copy + transform the three Swift layers into one single-module directory.
fn merge_sources(apple_dir: &Path, merged: &Path, autolink: &[String]) -> Result<()> {
    let ffi = apple_dir.join("Sources/KitharaFFI/KitharaFFI.swift");
    let content = fs::read_to_string(&ffi).with_context(|| format!("read {}", ffi.display()))?;
    fs::write(merged.join("KitharaFFI.swift"), transform_ffi(&content)?)?;

    for layer in ["Sources/Kithara", "Sources/KitharaRx"] {
        let dir = apple_dir.join(layer);
        for f in swift_files(&dir)? {
            let content =
                fs::read_to_string(&f).with_context(|| format!("read {}", f.display()))?;
            let name = f.file_name().context("layer source without a file name")?;
            if name.to_str() == Some("DrmSalt.swift") {
                continue;
            }
            fs::write(merged.join(Path::new(name)), transform_layer(&content)?)?;
        }
    }

    // Autolink stub: importing these system frameworks makes the static
    // framework carry `-framework` directives, so a consumer resolves the
    // Rust core's symbols with no extra link flags.
    let system_link = autolink
        .iter()
        .map(|fw| format!("import {fw}\n"))
        .collect::<String>();
    fs::write(merged.join("_SystemLink.swift"), system_link)?;
    Ok(())
}

/// Generated-binding transforms: hide the C module behind `internal import`,
/// demote the `UniFFI` scaffolding that publicly exposes `RustBuffer`, and
/// rename the two generated protocols that clash with the high-level ones.
fn transform_ffi(src: &str) -> Result<String> {
    const CONVERTER_PREFIXES: &[&str] = &[
        "public func FfiConverter",
        "public struct FfiConverter",
        "public enum FfiConverter",
        "public final class FfiConverter",
        "public class FfiConverter",
        "public var FfiConverter",
        "public let FfiConverter",
    ];
    let mut out = String::with_capacity(src.len());
    for line in src.lines() {
        if line == "import KitharaFFIInternal" {
            out.push_str("internal import KitharaFFIInternal\n");
            continue;
        }
        let demote = CONVERTER_PREFIXES.iter().any(|p| line.starts_with(p))
            || line.starts_with("public func uniffi")
            || line.starts_with("public func ffi_");
        if demote {
            if let Some(stripped) = line.strip_prefix("public ") {
                out.push_str(stripped);
            } else {
                out.push_str(line);
            }
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    let item_protocol = Regex::new(r"\bAudioPlayerItemProtocol\b")
        .context("compile AudioPlayerItemProtocol rename regex")?;
    let player_protocol = Regex::new(r"\bAudioPlayerProtocol\b")
        .context("compile AudioPlayerProtocol rename regex")?;
    let track_id = Regex::new(r"\bTrackId\b").context("compile TrackId rename regex")?;
    let out = item_protocol
        .replace_all(&out, "FfiAudioPlayerItemProtocol")
        .into_owned();
    let out = player_protocol
        .replace_all(&out, "FfiAudioPlayerProtocol")
        .into_owned();
    Ok(track_id.replace_all(&out, "FfiTrackId").into_owned())
}

/// High-level layer transforms: drop now-intra-module imports, drop the
/// re-export typealiases (self-referential after merge), strip the qualifier.
fn transform_layer(src: &str) -> Result<String> {
    let typealias_re = Regex::new(r"^\s*public typealias \w+ = KitharaFFI\.")
        .context("compile KitharaFFI typealias regex")?;
    let mut out = String::with_capacity(src.len());
    for line in src.lines() {
        if line == "import KitharaFFI" || line == "import Kithara" {
            continue;
        }
        if typealias_re.is_match(line) {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    let out = out.replace("KitharaFFI.TrackId", "FfiTrackId");
    Ok(out.replace("KitharaFFI.", ""))
}

/// Compile the merged module per arch, merge the Rust slice in, and assemble
/// the final `XCFramework`.
fn build_single_xcframework(
    merged: &Path,
    internal: &Path,
    rx_src: &Path,
    work: &Path,
    out: &Path,
    spec: &SingleFrameworkSpec,
    tools: &ToolsConfig,
) -> Result<()> {
    let ios_sdk = sdk_path("iphoneos", tools)?;
    let sim_sdk = sdk_path("iphonesimulator", tools)?;

    let mm_dev = internal.join("ios-arm64/Headers/KitharaFFIInternal");
    let mm_sim = internal.join(format!(
        "{}/Headers/KitharaFFIInternal",
        Consts::IOS_SIMULATOR_SLICE
    ));
    let rust_dev = internal.join("ios-arm64/libkithara_ffi.a");
    let rust_sim = internal.join(format!("{}/libkithara_ffi.a", Consts::IOS_SIMULATOR_SLICE));
    require_file(&mm_dev.join("module.modulemap"))?;
    require_file(&mm_sim.join("module.modulemap"))?;
    require_file(&rust_dev)?;
    require_file(&rust_sim)?;

    let msrc = swift_files(merged)?;
    let rx_files = swift_files_recursive(rx_src)?;

    let dev_out = work.join("dev");
    let sim_a_out = work.join("sim-a");
    let rx_dev_out = work.join("rx/dev");
    let rx_sim_a_out = work.join("rx/sim-a");

    let dt = &spec.deployment_target;
    let triple_dev = format!("arm64-apple-ios{dt}");
    let triple_sim_a = format!("arm64-apple-ios{dt}-simulator");

    let slices = [
        ArchBuild {
            module: &spec.framework_name,
            triple: &triple_dev,
            sdk: &ios_sdk,
            module_map: &mm_dev,
            rust_lib: &rust_dev,
            out: &dev_out,
            module_triple: "arm64-apple-ios",
            rx_out: &rx_dev_out,
        },
        ArchBuild {
            module: &spec.framework_name,
            triple: &triple_sim_a,
            sdk: &sim_sdk,
            module_map: &mm_sim,
            rust_lib: &rust_sim,
            out: &sim_a_out,
            module_triple: "arm64-apple-ios-simulator",
            rx_out: &rx_sim_a_out,
        },
    ];
    for slice in &slices {
        build_arch(slice, &msrc, &rx_files, tools)?;
    }

    let fw_ios = work.join("fw/ios");
    let fw_sim = work.join("fw/sim");
    assemble_framework(&fw_ios, "iPhoneOS", &[&dev_out], spec, tools)?;
    assemble_framework(&fw_sim, "iPhoneSimulator", &[&sim_a_out], spec, tools)?;

    let framework = format!("{}.framework", spec.framework_name);
    create_xcframework(
        &[&fw_ios.join(&framework), &fw_sim.join(&framework)],
        out,
        tools,
    )
}

/// Build one arch slice: temp `RxSwift` module, merged module, libtool merge.
fn build_arch(
    arch: &ArchBuild,
    msrc: &[PathBuf],
    rx_files: &[PathBuf],
    tools: &ToolsConfig,
) -> Result<()> {
    fs::create_dir_all(arch.rx_out)?;
    fs::create_dir_all(arch.out)?;
    println!("==> Compiling {} ({})", arch.module, arch.module_triple);
    build_rxswift(arch.triple, arch.sdk, rx_files, arch.rx_out, tools)?;
    build_merged(arch, msrc, tools)?;
    libtool_merge(
        &arch.out.join(format!("lib{}Swift.a", arch.module)),
        arch.rust_lib,
        &arch.out.join(format!("{}.a", arch.module)),
        tools,
    )
}

/// Build a temporary `RxSwift` static module for one arch (the consumer ships
/// its own `RxSwift`; this only resolves the module at compile time).
fn build_rxswift(
    triple: &str,
    sdk: &Path,
    rx_files: &[PathBuf],
    rx_out: &Path,
    tools: &ToolsConfig,
) -> Result<()> {
    let mut cmd = Command::new(tools.program("xcrun"));
    cmd.args([
        "swiftc",
        "-emit-module",
        "-emit-library",
        "-static",
        "-module-name",
        "RxSwift",
        "-emit-module-path",
    ])
    .arg(rx_out.join("RxSwift.swiftmodule"))
    .arg("-target")
    .arg(triple)
    .arg("-sdk")
    .arg(sdk);
    for f in rx_files {
        cmd.arg(f);
    }
    cmd.arg("-o").arg(rx_out.join("libRxSwift.a"));
    run_quiet(&mut cmd, "build RxSwift module")
}

/// Compile the merged single module with library evolution, emitting a
/// canonically-named `.swiftinterface`.
fn build_merged(arch: &ArchBuild, msrc: &[PathBuf], tools: &ToolsConfig) -> Result<()> {
    let mut cmd = Command::new(tools.program("xcrun"));
    cmd.args([
        "swiftc",
        "-emit-module",
        "-emit-library",
        "-static",
        "-enable-library-evolution",
        "-module-name",
        arch.module,
        "-emit-module-path",
    ])
    .arg(arch.out.join(format!("{}.swiftmodule", arch.module)))
    .arg("-emit-module-interface-path")
    .arg(
        arch.out
            .join(format!("{}.swiftinterface", arch.module_triple)),
    )
    .arg("-target")
    .arg(arch.triple)
    .arg("-sdk")
    .arg(arch.sdk)
    .arg("-Xcc")
    .arg(format!(
        "-fmodule-map-file={}",
        arch.module_map.join("module.modulemap").display()
    ))
    .arg("-I")
    .arg(arch.module_map)
    .arg("-I")
    .arg(arch.rx_out);
    for f in msrc {
        cmd.arg(f);
    }
    cmd.arg("-o")
        .arg(arch.out.join(format!("lib{}Swift.a", arch.module)));
    run_quiet(&mut cmd, "compile merged module")
}

/// Merge the Swift static lib and the Rust static lib into one archive.
fn libtool_merge(
    swift_lib: &Path,
    rust_lib: &Path,
    out_lib: &Path,
    tools: &ToolsConfig,
) -> Result<()> {
    let program = tools.program("libtool");
    let mut cmd = Command::new(program);
    cmd.arg("-static")
        .arg("-o")
        .arg(out_lib)
        .arg(swift_lib)
        .arg(rust_lib);
    run_quiet(&mut cmd, &format!("{program} merge"))
}

/// Assemble a `.framework` for one platform from one or more arch slices.
fn assemble_framework(
    fw_dir: &Path,
    platform: &str,
    slices: &[&Path],
    spec: &SingleFrameworkSpec,
    tools: &ToolsConfig,
) -> Result<()> {
    let name = &spec.framework_name;
    let fw = fw_dir.join(format!("{name}.framework"));
    if fw.exists() {
        fs::remove_dir_all(&fw)?;
    }
    let modules = fw.join(format!("Modules/{name}.swiftmodule"));
    fs::create_dir_all(&modules)?;

    let program = tools.program("lipo");
    let mut lipo = Command::new(program);
    lipo.arg("-create");
    for slice in slices {
        lipo.arg(slice.join(format!("{name}.a")));
    }
    lipo.arg("-output").arg(fw.join(name));
    run_quiet(&mut lipo, &format!("{program} framework binary"))?;

    for slice in slices {
        let mut module_triple = None;
        for entry in fs::read_dir(slice)? {
            let path = entry?.path();
            if path.extension().and_then(|e| e.to_str()) == Some("swiftinterface") {
                let file_name = path.file_name().context("interface without a name")?;
                fs::copy(&path, modules.join(file_name))?;
                let file_name_str = file_name.to_string_lossy();
                if !file_name_str.contains(".private.") {
                    let stem = path
                        .file_stem()
                        .context("interface without a stem")?
                        .to_string_lossy()
                        .into_owned();
                    module_triple = Some(stem);
                }
            }
        }
        let module_triple = module_triple
            .with_context(|| format!("no public swiftinterface found in {}", slice.display()))?;
        fs::copy(
            slice.join(format!("{name}.swiftmodule")),
            modules.join(format!("{module_triple}.swiftmodule")),
        )?;
    }

    write_info_plist(&fw.join("Info.plist"), platform, spec)
}

/// Bundle the per-platform `.framework`s into the final `XCFramework`.
fn create_xcframework(frameworks: &[&Path], out: &Path, tools: &ToolsConfig) -> Result<()> {
    let mut cmd = Command::new(tools.program("xcodebuild"));
    cmd.arg("-create-xcframework");
    for fw in frameworks {
        cmd.arg("-framework").arg(fw);
    }
    cmd.arg("-output").arg(out);
    run_quiet(&mut cmd, "create-xcframework")
}

/// Fail unless the shipped public interface is clean and inheritance is on.
///
/// Project-agnostic invariants: the `UniFFI` runtime type `RustBuffer` must
/// not appear in any public `.swiftinterface` (proof the C-module scaffolding
/// was fully demoted), and at least one `open class` must be present (proof
/// the single-module build kept subclassable types).
fn verify_single(out: &Path) -> Result<()> {
    let interfaces = public_swiftinterfaces(out)?;
    if interfaces.is_empty() {
        bail!("no public swiftinterfaces found under {}", out.display());
    }
    let mut leak_refs = 0;
    let mut has_open_class = false;
    for iface in &interfaces {
        let content =
            fs::read_to_string(iface).with_context(|| format!("read {}", iface.display()))?;
        leak_refs += content.matches("RustBuffer").count();
        has_open_class |= content.contains("open class ");
    }
    if leak_refs > 0 {
        bail!(
            "public interface leaks {leak_refs} RustBuffer reference(s) — UniFFI scaffolding not fully demoted"
        );
    }
    if !has_open_class {
        bail!("public interface has no `open class` — inheritance not enabled");
    }
    println!("==> Verified: 0 RustBuffer leaks; open classes present in public interface");
    Ok(())
}

/// `xcrun --sdk <sdk> --show-sdk-path`, resolved to the directory it names.
///
/// `xcrun` answers with the versioned name, which is a symlink, while
/// `xcodebuild` resolves `SDKROOT` to the directory behind it. Both spellings
/// reached one compilation and Swift's explicit module build registered the
/// SDK's own modules under each: `iPhoneSimulator26.5.sdk/…/module.modulemap:
/// error: redefinition of module 'SwiftShims'`, previously defined under
/// `iPhoneSimulator.sdk/…`. Resolving here leaves one spelling downstream.
fn sdk_path(sdk: &str, tools: &ToolsConfig) -> Result<PathBuf> {
    let program = tools.program("xcrun");
    let output = Command::new(program)
        .args(["--sdk", sdk, "--show-sdk-path"])
        .output()
        .with_context(|| format!("{program} --show-sdk-path {sdk}"))?;
    if !output.status.success() {
        bail!("{program} --show-sdk-path {sdk} failed");
    }
    let reported = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim().to_string());
    reported
        .canonicalize()
        .with_context(|| format!("resolving {sdk} SDK path {}", reported.display()))
}

/// Extract a single arch from a fat static lib.
fn lipo_create(thin: &[PathBuf], out: &Path, tools: &ToolsConfig) -> Result<()> {
    let program = tools.program("lipo");
    let mut cmd = Command::new(program);
    cmd.arg("-create").args(thin).arg("-output").arg(out);
    run_quiet(&mut cmd, &format!("{program} create"))
}

fn lipo_thin(fat: &Path, arch: &str, out: &Path, tools: &ToolsConfig) -> Result<()> {
    let program = tools.program("lipo");
    let mut cmd = Command::new(program);
    cmd.arg(fat).arg("-thin").arg(arch).arg("-output").arg(out);
    run_quiet(&mut cmd, &format!("{program} thin"))
}

fn require_dir(path: &Path) -> Result<()> {
    if !path.is_dir() {
        bail!("required directory is missing: {}", path.display());
    }
    Ok(())
}

fn require_file(path: &Path) -> Result<()> {
    if !path.is_file() {
        bail!("required file is missing: {}", path.display());
    }
    Ok(())
}

/// Top-level `.swift` files in `dir`, sorted.
fn swift_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in fs::read_dir(dir).with_context(|| format!("read_dir {}", dir.display()))? {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) == Some("swift") {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

/// All `.swift` files under `dir` (recursive), sorted.
fn swift_files_recursive(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    collect_swift(dir, &mut files)?;
    files.sort();
    Ok(files)
}

fn public_swiftinterfaces(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    collect_public_swiftinterfaces(dir, &mut files)?;
    files.sort();
    Ok(files)
}

fn collect_swift(dir: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("read_dir {}", dir.display()))? {
        let path = entry?.path();
        if path.is_dir() {
            collect_swift(&path, files)?;
        } else if path.extension().and_then(|e| e.to_str()) == Some("swift") {
            files.push(path);
        }
    }
    Ok(())
}

fn collect_public_swiftinterfaces(dir: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("read_dir {}", dir.display()))? {
        let path = entry?.path();
        if path.is_dir() {
            collect_public_swiftinterfaces(&path, files)?;
        } else if path.extension().and_then(|e| e.to_str()) == Some("swiftinterface") {
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .context("swiftinterface without UTF-8 file name")?;
            if !name.contains(".private.") {
                files.push(path);
            }
        }
    }
    Ok(())
}

/// Write the single-platform `.framework` `Info.plist` via the `plist`
/// crate (typed struct -> XML), driven entirely by the spec.
fn write_info_plist(path: &Path, platform: &str, spec: &SingleFrameworkSpec) -> Result<()> {
    let info = FrameworkInfoPlist {
        executable: spec.framework_name.clone(),
        identifier: spec.bundle_id.clone(),
        info_dictionary_version: "6.0".to_string(),
        name: spec.framework_name.clone(),
        package_type: "FMWK".to_string(),
        short_version: spec.short_version.clone(),
        bundle_version: spec.bundle_version.clone(),
        minimum_os: spec.deployment_target.clone(),
        supported_platforms: vec![platform.to_string()],
    };
    plist::to_file_xml(path, &info)
        .with_context(|| format!("write Info.plist to {}", path.display()))?;
    Ok(())
}

/// Run a command, surfacing captured output only on failure.
fn run_quiet(cmd: &mut Command, what: &str) -> Result<()> {
    let output = cmd.output().with_context(|| format!("spawn {what}"))?;
    if !output.status.success() {
        bail!(
            "{what} failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

fn run_app(
    simulator: Option<&str>,
    scheme: Option<&str>,
    profile: crate::BuildProfile,
    debug: bool,
    skip_framework: bool,
    apple: &AppleConfig,
    tools: &ToolsConfig,
) -> Result<()> {
    let scheme = match scheme {
        Some(scheme) => scheme.to_owned(),
        None => require_apple_str(&apple.default_scheme, "default_scheme")?.to_owned(),
    };
    let simulator = match simulator {
        Some(simulator) => simulator.to_owned(),
        None => require_apple_str(&apple.default_simulator, "default_simulator")?.to_owned(),
    };
    let bundle_id = require_apple_str(&apple.demo_bundle_id, "demo_bundle_id")?;

    let metadata = MetadataCommand::new()
        .exec()
        .context("failed to read cargo metadata")?;
    let workspace_root = metadata.workspace_root.as_std_path().to_path_buf();
    let demo_dir = workspace_root.join("apple/Examples/KitharaDemo");
    let xcodeproj = demo_dir.join("KitharaDemo.xcodeproj");
    if !xcodeproj.exists() {
        bail!("KitharaDemo.xcodeproj not found at {}", xcodeproj.display());
    }

    if !skip_framework {
        // The demo links `KitharaFFIInternal.xcframework`, so the
        // XCFramework must exist before xcodebuild can resolve the
        // package graph; refresh it the same way `run_build` does.
        run_build(profile, None, tools)?;
    }

    let uuid = resolve_simulator_uuid(&simulator, tools)?;
    boot_simulator(&uuid, tools)?;
    open_simulator_app();

    let configuration = match profile {
        crate::BuildProfile::Release => "Release",
        crate::BuildProfile::Debug => "Debug",
    };

    println!("==> Building {scheme} ({configuration}) for simulator {simulator}");
    let destination = format!("platform=iOS Simulator,id={uuid}");
    let program = tools.program("xcodebuild");
    let mut build = Command::new(program);
    build
        .args([
            "-project",
            xcodeproj.to_str().context("xcodeproj path is not UTF-8")?,
            "-scheme",
            &scheme,
            "-configuration",
            configuration,
            "-destination",
            &destination,
            "-derivedDataPath",
            "build/DerivedData",
            "build",
        ])
        .current_dir(&demo_dir);
    let status = build
        .status()
        .with_context(|| format!("failed to run {program}"))?;
    if !status.success() {
        bail!("{program} failed");
    }

    let app_path = locate_built_app(&demo_dir, &scheme, configuration)?;

    println!("==> Installing {} on simulator", app_path.display());
    let program = tools.program("xcrun");
    let status = Command::new(program)
        .args([
            "simctl",
            "install",
            &uuid,
            app_path.to_str().context(".app path is not UTF-8")?,
        ])
        .status()
        .with_context(|| format!("failed to run `{program} simctl install`"))?;
    if !status.success() {
        bail!("simctl install failed");
    }

    println!("==> Launching {bundle_id}");
    let mut launch = Command::new(program);
    launch.args(["simctl", "launch"]);
    if debug {
        // Suspends the app on entry so `lldb -p <pid>` (or Zed's
        // CodeLLDB "attach" config) can hook into it before any user
        // code runs.
        launch.arg("--wait-for-debugger");
    }
    launch.args([&uuid, bundle_id]);
    let output = launch
        .output()
        .with_context(|| format!("failed to run `{program} simctl launch`"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("simctl launch failed: {stderr}");
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    println!("{}", stdout.trim_end());

    if debug {
        println!();
        println!("==> App is suspended waiting for a debugger.");
        if let Some(pid) = parse_launch_pid(&stdout) {
            println!("    PID: {pid}");
            println!("    Attach via:                 lldb -p {pid}");
            println!("    Or in Zed: pick the `iOS demo: attach (debug)` configuration.");
        } else {
            println!("    Could not parse the PID from `simctl launch` output.");
            println!(
                "    Find it manually: {program} simctl spawn {uuid} ps -A | grep KitharaDemo"
            );
        }
    }

    Ok(())
}

fn resolve_simulator_uuid(name_or_uuid: &str, tools: &ToolsConfig) -> Result<String> {
    // If the argument already looks like a UUID, accept it as-is.
    if name_or_uuid.len() == 36 && name_or_uuid.chars().filter(|c| *c == '-').count() == 4 {
        return Ok(name_or_uuid.to_owned());
    }
    let program = tools.program("xcrun");
    let output = Command::new(program)
        .args(["simctl", "list", "devices", "available"])
        .output()
        .with_context(|| format!("failed to run `{program} simctl list devices available`"))?;
    if !output.status.success() {
        bail!("simctl list devices failed");
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        // Lines look like:
        //     iPhone 17 Pro Max (D18BAAE9-CEF2-44F6-95C5-ADBE8A027C6C) (Shutdown)
        let trimmed = line.trim();
        if !trimmed.starts_with(name_or_uuid) {
            continue;
        }
        if let Some(open) = trimmed.find('(')
            && let Some(close) = trimmed[open + 1..].find(')')
        {
            let uuid = &trimmed[open + 1..open + 1 + close];
            if uuid.len() == 36 {
                return Ok(uuid.to_owned());
            }
        }
    }
    bail!("simulator '{name_or_uuid}' not found in `{program} simctl list`")
}

fn boot_simulator(uuid: &str, tools: &ToolsConfig) -> Result<()> {
    // `simctl boot` is a no-op (and exits 149 / "Unable to boot") when
    // the device is already booted; treat that as success.
    let program = tools.program("xcrun");
    let status = Command::new(program)
        .args(["simctl", "boot", uuid])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .with_context(|| format!("failed to run `{program} simctl boot`"))?;
    let _ = status;
    Ok(())
}

fn open_simulator_app() {
    // Bringing Simulator.app to the foreground is a UX nicety, not a
    // correctness requirement; failures are non-fatal.
    let _ = Command::new("open").args(["-a", "Simulator"]).status();
}

fn locate_built_app(demo_dir: &Path, scheme: &str, configuration: &str) -> Result<PathBuf> {
    let products_dir = demo_dir
        .join("build/DerivedData/Build/Products")
        .join(format!("{configuration}-iphonesimulator"));

    // The xcodegen project splits per-platform schemes (`KitharaDemo_iOS`,
    // `KitharaDemo_macOS`) but keeps a single `PRODUCT_NAME` → there is
    // exactly one `*.app` per products dir, and its name is the
    // PRODUCT_NAME, not the scheme. Try the common-case match first,
    // then fall back to "any .app in the directory" so the lookup
    // survives PRODUCT_NAME tweaks.
    if let Some(direct) = first_existing_app(&products_dir, scheme) {
        return Ok(direct);
    }
    let entries = fs::read_dir(&products_dir)
        .with_context(|| format!("read_dir {}", products_dir.display()))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "app") {
            return Ok(path);
        }
    }
    bail!(
        "no .app bundle found under {} (built {scheme}/{configuration})",
        products_dir.display()
    )
}

fn first_existing_app(products_dir: &Path, scheme: &str) -> Option<PathBuf> {
    // Match the `*_iOS` / `*_macOS` xcodegen split: scheme suffix is
    // dropped to recover the PRODUCT_NAME most projects use.
    let stripped = scheme
        .strip_suffix("_iOS")
        .or_else(|| scheme.strip_suffix("_macOS"))
        .unwrap_or(scheme);
    for candidate in [scheme, stripped] {
        let path = products_dir.join(format!("{candidate}.app"));
        if path.exists() {
            return Some(path);
        }
    }
    None
}

fn parse_launch_pid(stdout: &str) -> Option<u32> {
    // `simctl launch` prints `com.kithara.demo: 12345` on success.
    stdout
        .lines()
        .find_map(|line| line.split(':').nth(1)?.trim().parse::<u32>().ok())
}

fn require_apple_str<'a>(value: &'a str, key: &str) -> Result<&'a str> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        bail!("ext.apple.{key} is not set; fill in the [ext.apple] section of .config/xtask.toml");
    }
    Ok(trimmed)
}

fn require_apple_needles<'a>(needles: &'a [String], key: &str) -> Result<&'a [String]> {
    if needles.is_empty() {
        bail!("ext.apple.{key} is not set; fill in the [ext.apple] section of .config/xtask.toml");
    }
    Ok(needles)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transform_ffi_keeps_track_id_internal_to_ffi_namespace() {
        let src = "\
public protocol AudioPlayerItemProtocol {
    func audioId() -> TrackId
}
public typealias TrackId = UInt64
public struct FfiConverterTypeTrackId {
    public static func lift(_ value: UInt64) throws -> TrackId { value }
}
";
        let out = transform_ffi(src).unwrap();

        assert!(
            out.contains("public protocol FfiAudioPlayerItemProtocol"),
            "{out}"
        );
        assert!(out.contains("func audioId() -> FfiTrackId"), "{out}");
        assert!(
            out.contains("public typealias FfiTrackId = UInt64"),
            "{out}"
        );
        assert!(out.contains("FfiConverterTypeTrackId"), "{out}");
        assert!(!out.contains("typealias TrackId = UInt64"), "{out}");
    }

    #[test]
    fn generated_swift_has_no_trailing_whitespace() {
        let src = "public struct Value {  \n\tlet id: UInt64\t\n}\n";

        assert_eq!(
            normalize_generated_swift(src),
            "public struct Value {\n\tlet id: UInt64\n}\n"
        );
    }

    /// Every process this module starts names an owner. Three spellings stay
    /// literal on purpose: `cargo` and `rustc` are toolchain ground, and `open`
    /// is a macOS system binary. Everything else resolves through the table,
    /// `symbol_tool` included.
    ///
    /// Accounting for the argument rather than searching for one spelling is
    /// what makes this bite: a constant or a `PathBuf::from` evades a text
    /// search for `Command::new("xcrun")` and fails here. The one hoisted
    /// local allowed is `program`, which lets a diagnostic interpolate the
    /// spelling that actually ran; its binding is held to the same owners, so
    /// the hoist cannot smuggle a literal past the first census.
    #[test]
    fn every_process_this_module_starts_has_a_declared_owner() {
        let source = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/apple.rs"))
            .expect("this source is readable");
        let (production, _) = source
            .split_once("\n#[cfg(test)]")
            .expect("this source carries a test module to cut the production half at");

        let resolvers = ["tools.program(", "symbol_tool("];
        let owned = [
            "tools.program(",
            "symbol_tool(",
            "program)",
            "\"cargo\"",
            "\"open\"",
            "\"rustc\"",
        ];
        let opener = "Command::new(";
        let mut spawns = 0;
        let mut unowned: Vec<&str> = Vec::new();
        for (at, _) in production.match_indices(opener) {
            if production[..at].ends_with(|char: char| char.is_alphanumeric() || char == '_') {
                continue;
            }
            spawns += 1;
            let argument = &production[at + opener.len()..];
            if !owned.iter().any(|form| argument.starts_with(form)) {
                unowned.push(argument.lines().next().unwrap_or(argument));
            }
        }

        let binder = "let program = ";
        let mut bindings = 0;
        let mut unresolved: Vec<&str> = Vec::new();
        for (at, _) in production.match_indices(binder) {
            bindings += 1;
            let source = &production[at + binder.len()..];
            if !resolvers.iter().any(|form| source.starts_with(form)) {
                unresolved.push(source.lines().next().unwrap_or(source));
            }
        }

        assert!(spawns > 0, "this module starts processes");
        assert!(bindings > 0, "this module binds resolved programs");
        assert!(
            unowned.is_empty(),
            "these spawns name a program no owner declares: {unowned:?}"
        );
        assert!(
            unresolved.is_empty(),
            "these `program` bindings skip the table: {unresolved:?}"
        );
    }

    #[test]
    fn transform_layer_preserves_public_track_id_alias() {
        let src = "\
import KitharaFFI
public typealias TrackId = Int
let ffiTrackId: KitharaFFI.TrackId
public typealias SeekCallback = KitharaFFI.SeekCallback
";
        let out = transform_layer(src).unwrap();

        assert!(out.contains("public typealias TrackId = Int"), "{out}");
        assert!(out.contains("let ffiTrackId: FfiTrackId"), "{out}");
        assert!(!out.contains("KitharaFFI."), "{out}");
        assert!(!out.contains("SeekCallback ="), "{out}");
    }
}
