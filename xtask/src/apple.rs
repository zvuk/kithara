use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use cargo_metadata::MetadataCommand;
use plist::Value as PlistValue;
use regex::Regex;

use crate::common::project::ProjectConfig;

/// Module constants for `cargo xtask apple run`. Grouped per the
/// `style.multiple-private-module-consts` lint.
struct Consts;
impl Consts {
    /// Default simulator picked when `--simulator` is omitted. Mirrors
    /// the device installed in the team's typical Xcode 16 setup; CLI
    /// override is available for everything else.
    const DEFAULT_SIMULATOR: &'static str = "iPhone 17 Pro Max";
    /// Default Xcode scheme — the iOS slice of the demo project.
    const DEFAULT_SCHEME: &'static str = "KitharaDemo_iOS";
    /// Bundle id installed by the demo target.
    const DEMO_BUNDLE_ID: &'static str = "com.kithara.demo";

    /// Banned substrings: presence in any apple-slice `libkithara_ffi.a`
    /// means the build leaked a software decoder backend that should stay
    /// behind the `symphonia` / desktop feature gate. Note:
    /// `symphonia_core`, `symphonia_format_isomp4`, `symphonia_common`,
    /// `symphonia_metadata` are *parser* infrastructure (fMP4 box walker,
    /// shared types) that the Apple decoder path reuses for demuxing —
    /// they're not software decoders and are allowed.
    const BANNED_SYMBOL_NEEDLES: &[&str] = &[
        "symphonia_bundle_",     // symphonia-bundle-flac, symphonia-bundle-mp3, …
        "symphonia_codec_",      // symphonia-codec-aac, symphonia-codec-alac, …
        "symphonia_format_caf",  // CAF — Apple uses AppleAudioFileDemuxer instead
        "symphonia_format_mkv",  // Matroska — not used on Apple
        "symphonia_format_ogg",  // Ogg — not used on Apple
        "symphonia_format_riff", // RIFF/WAV — Apple uses AppleAudioFileDemuxer
        "fdk_aac",
    ];
    /// Positive proof — at least one of these must appear in every slice
    /// to confirm the Apple HW dispatcher is actually linked in.
    const APPLE_PROOF_NEEDLES: &[&str] = &["AppleCodec", "AppleAudioFileDemuxer"];
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
    /// `MinimumOSVersion` / build target (e.g. `16.0`).
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
    active: bool,
}

impl HakariDisableGuard {
    fn disable(workspace_root: &Path) -> Result<Self> {
        let manifest = workspace_root.join("crates/kithara-workspace-hack/Cargo.toml");
        let original_manifest = fs::read_to_string(&manifest)
            .with_context(|| format!("read {}", manifest.display()))?;
        let mut guard = Self {
            manifest,
            original_manifest,
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
    /// Build release Apple artifacts, strip/audit them, zip them, and print
    /// the SPM checksum for the Rust `XCFramework` binary target.
    Release,
}

pub(crate) fn run(cmd: AppleCommand) -> Result<()> {
    match cmd {
        AppleCommand::Build { profile } => run_build(profile),
        AppleCommand::Single { profile } => run_single(profile),
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
        ),
        AppleCommand::Audit { path } => audit_symbols(&path),
        AppleCommand::Release => run_release(),
    }
}

/// Run `nm` on every slice's static lib and fail if any
/// software-backend symbol survived linking or the Apple dispatcher
/// went missing.
fn audit_symbols(xcframework_dir: &Path) -> Result<()> {
    if !xcframework_dir.is_dir() {
        bail!(
            "xcframework path does not exist or is not a directory: {}",
            xcframework_dir.display()
        );
    }
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
        let output = Command::new(symbol_tool())
            .arg(&lib)
            .output()
            .with_context(|| format!("invoke symbol audit on {}", lib.display()))?;
        let symbols = String::from_utf8_lossy(&output.stdout);
        let strings = archive_strings(&lib)?;
        for needle in Consts::BANNED_SYMBOL_NEEDLES {
            let count = symbols.matches(needle).count() + strings.matches(needle).count();
            if count > 0 {
                errors.push(format!(
                    "slice `{slice}` leaked {count} `{needle}` symbols — \
                     software-backend dep must stay behind the symphonia feature gate"
                ));
            }
        }
        let has_apple_proof = Consts::APPLE_PROOF_NEEDLES
            .iter()
            .any(|n| symbols.contains(n) || strings.contains(n));
        if !has_apple_proof {
            let proof = Consts::APPLE_PROOF_NEEDLES;
            errors.push(format!(
                "slice `{slice}` missing Apple-backend proof symbols \
                 ({proof:?}) — AppleCodec not linked?"
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

fn symbol_tool() -> PathBuf {
    rust_tool("llvm-nm").unwrap_or_else(|| PathBuf::from("nm"))
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

fn archive_strings(lib: &Path) -> Result<String> {
    let output = Command::new("strings")
        .arg(lib)
        .output()
        .with_context(|| format!("invoke strings on {}", lib.display()))?;
    if !output.status.success() {
        bail!("strings failed for {}", lib.display());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn run_build(profile: crate::BuildProfile) -> Result<()> {
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
    cmd.args(["swift", "package", "-p", "ios", "macos", "-n", "KitharaFFI"]);
    if matches!(profile, crate::BuildProfile::Release) {
        cmd.arg("--release");
    }
    cmd.args([
        "--lib-type",
        "static",
        // Device build: drop default features so `symphonia` is absent —
        // the Apple AudioToolbox backend is the sole decoder on-device.
        "--no-default-features",
        "-F",
        "uniffi,apple,dev",
        "--swift-tools-version",
        "6.0",
        "-y",
    ]);
    cmd.current_dir(&crate_dir);
    cmd.env("IPHONEOS_DEPLOYMENT_TARGET", "16.0");

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
    keep_arm64_ios_simulator_only(&xcf_dst)?;
    if matches!(profile, crate::BuildProfile::Release) {
        strip_xcframework(&xcf_dst)?;
    }

    if let Some(parent) = swift_dst.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(&swift_src, &swift_dst)
        .with_context(|| format!("copy {} -> {}", swift_src.display(), swift_dst.display()))?;

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

    if let Some(guard) = &mut hakari_guard {
        guard.restore()?;
    }

    Ok(())
}

fn run_release() -> Result<()> {
    let metadata = MetadataCommand::new()
        .exec()
        .context("failed to read cargo metadata")?;
    let root = metadata.workspace_root.as_std_path().to_path_buf();
    let project = ProjectConfig::load(&root)?;
    let release = &project.release;
    if release.asset.trim().is_empty() {
        bail!("release.asset is not set in .config/xtask.toml");
    }
    if release.single_asset.trim().is_empty() {
        bail!("release.single_asset is not set in .config/xtask.toml");
    }

    let apple_dir = root.join("apple");
    let internal = apple_dir.join("KitharaFFIInternal.xcframework");

    run_build(crate::BuildProfile::Release)?;
    audit_symbols(&internal)?;
    run_single(crate::BuildProfile::Release)?;

    let tmp = env::temp_dir();
    let internal_zip = tmp.join(&release.asset);
    let single_zip = tmp.join(&release.single_asset);
    zip_dir(&apple_dir, "KitharaFFIInternal.xcframework", &internal_zip)?;

    let single_dir = release
        .single_asset
        .strip_suffix(".zip")
        .context("release.single_asset must end with .zip")?;
    zip_dir(&apple_dir.join("dist"), single_dir, &single_zip)?;

    let checksum = swift_checksum(&internal_zip)?;
    let checksum_file = tmp.join(format!("{}.sha256", release.asset));
    fs::write(&checksum_file, format!("{checksum}\n"))
        .with_context(|| format!("write {}", checksum_file.display()))?;

    println!("==> Release artifacts:");
    println!("    {}", internal_zip.display());
    println!("    {}", single_zip.display());
    println!("    {}", checksum_file.display());
    println!("==> SPM checksum: {checksum}");
    Ok(())
}

fn strip_xcframework(xcframework: &Path) -> Result<()> {
    require_dir(xcframework)?;
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
        let status = Command::new("strip")
            .args(["-S", "-x"])
            .arg(&lib)
            .status()
            .with_context(|| format!("strip {}", lib.display()))?;
        if !status.success() {
            bail!("strip failed for {}", lib.display());
        }
    }
    Ok(())
}

fn keep_arm64_ios_simulator_only(xcframework: &Path) -> Result<()> {
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
        lipo_thin(&lib, "arm64", &tmp)?;
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

fn zip_dir(parent: &Path, directory_name: &str, output: &Path) -> Result<()> {
    let source = parent.join(directory_name);
    require_dir(&source)?;
    if output.exists() {
        fs::remove_file(output).with_context(|| format!("remove {}", output.display()))?;
    }
    println!("==> Zipping {} -> {}", source.display(), output.display());
    let status = Command::new("zip")
        .args(["-r", "-y"])
        .arg(output)
        .arg(directory_name)
        .current_dir(parent)
        .status()
        .with_context(|| format!("zip {}", source.display()))?;
    if !status.success() {
        bail!("zip failed for {}", source.display());
    }
    Ok(())
}

fn swift_checksum(zip: &Path) -> Result<String> {
    let output = Command::new("swift")
        .args(["package", "compute-checksum"])
        .arg(zip)
        .output()
        .context("run swift package compute-checksum")?;
    if !output.status.success() {
        bail!(
            "swift package compute-checksum failed: {}",
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
fn run_single(profile: crate::BuildProfile) -> Result<()> {
    let metadata = MetadataCommand::new()
        .exec()
        .context("failed to read cargo metadata")?;
    let spec = load_spec(&metadata)?;
    let root = metadata.workspace_root.as_std_path().to_path_buf();
    let apple_dir = root.join("apple");
    let internal = apple_dir.join("KitharaFFIInternal.xcframework");

    if !internal.exists() {
        println!("==> KitharaFFIInternal.xcframework not found — building it first");
        run_build(profile)?;
    }
    require_dir(&internal)?;
    keep_arm64_ios_simulator_only(&internal)?;
    if matches!(profile, crate::BuildProfile::Release) {
        strip_xcframework(&internal)?;
    }

    let rx_src = resolve_rxswift(&root)?;
    println!("==> RxSwift source: {}", rx_src.display());

    let temp = TempWorkDir::create("kithara-apple-single")?;
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

    build_single_xcframework(&merged, &internal, &rx_src, &work, &out, &spec)?;
    verify_single(&out)?;

    println!("==> Done!");
    println!("==> Single XCFramework: {}", out.display());
    Ok(())
}

/// Resolve the pinned `RxSwift` checkout via `SwiftPM` (the merged module imports
/// `RxSwift`, so its module must exist at compile time).
fn resolve_rxswift(root: &Path) -> Result<PathBuf> {
    println!("==> Resolving RxSwift via SwiftPM");
    let status = Command::new("swift")
        .args(["package", "resolve"])
        .current_dir(root)
        .env("KITHARA_LOCAL_DEV", "1")
        .status()
        .context("failed to run swift package resolve")?;
    if !status.success() {
        bail!("swift package resolve failed");
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
) -> Result<()> {
    let ios_sdk = sdk_path("iphoneos")?;
    let sim_sdk = sdk_path("iphonesimulator")?;

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
        build_arch(slice, &msrc, &rx_files)?;
    }

    let fw_ios = work.join("fw/ios");
    let fw_sim = work.join("fw/sim");
    assemble_framework(&fw_ios, "iPhoneOS", &[&dev_out], spec)?;
    assemble_framework(&fw_sim, "iPhoneSimulator", &[&sim_a_out], spec)?;

    let framework = format!("{}.framework", spec.framework_name);
    create_xcframework(&[&fw_ios.join(&framework), &fw_sim.join(&framework)], out)
}

/// Build one arch slice: temp `RxSwift` module, merged module, libtool merge.
fn build_arch(arch: &ArchBuild, msrc: &[PathBuf], rx_files: &[PathBuf]) -> Result<()> {
    fs::create_dir_all(arch.rx_out)?;
    fs::create_dir_all(arch.out)?;
    println!("==> Compiling {} ({})", arch.module, arch.module_triple);
    build_rxswift(arch.triple, arch.sdk, rx_files, arch.rx_out)?;
    build_merged(arch, msrc)?;
    libtool_merge(
        &arch.out.join(format!("lib{}Swift.a", arch.module)),
        arch.rust_lib,
        &arch.out.join(format!("{}.a", arch.module)),
    )
}

/// Build a temporary `RxSwift` static module for one arch (the consumer ships
/// its own `RxSwift`; this only resolves the module at compile time).
fn build_rxswift(triple: &str, sdk: &Path, rx_files: &[PathBuf], rx_out: &Path) -> Result<()> {
    let mut cmd = Command::new("xcrun");
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
fn build_merged(arch: &ArchBuild, msrc: &[PathBuf]) -> Result<()> {
    let mut cmd = Command::new("xcrun");
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
fn libtool_merge(swift_lib: &Path, rust_lib: &Path, out_lib: &Path) -> Result<()> {
    let mut cmd = Command::new("libtool");
    cmd.arg("-static")
        .arg("-o")
        .arg(out_lib)
        .arg(swift_lib)
        .arg(rust_lib);
    run_quiet(&mut cmd, "libtool merge")
}

/// Assemble a `.framework` for one platform from one or more arch slices.
fn assemble_framework(
    fw_dir: &Path,
    platform: &str,
    slices: &[&Path],
    spec: &SingleFrameworkSpec,
) -> Result<()> {
    let name = &spec.framework_name;
    let fw = fw_dir.join(format!("{name}.framework"));
    if fw.exists() {
        fs::remove_dir_all(&fw)?;
    }
    let modules = fw.join(format!("Modules/{name}.swiftmodule"));
    fs::create_dir_all(&modules)?;

    let mut lipo = Command::new("lipo");
    lipo.arg("-create");
    for slice in slices {
        lipo.arg(slice.join(format!("{name}.a")));
    }
    lipo.arg("-output").arg(fw.join(name));
    run_quiet(&mut lipo, "lipo framework binary")?;

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
fn create_xcframework(frameworks: &[&Path], out: &Path) -> Result<()> {
    let mut cmd = Command::new("xcodebuild");
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

/// `xcrun --sdk <sdk> --show-sdk-path`.
fn sdk_path(sdk: &str) -> Result<PathBuf> {
    let output = Command::new("xcrun")
        .args(["--sdk", sdk, "--show-sdk-path"])
        .output()
        .with_context(|| format!("xcrun --show-sdk-path {sdk}"))?;
    if !output.status.success() {
        bail!("xcrun --show-sdk-path {sdk} failed");
    }
    Ok(PathBuf::from(
        String::from_utf8_lossy(&output.stdout).trim().to_string(),
    ))
}

/// Extract a single arch from a fat static lib.
fn lipo_thin(fat: &Path, arch: &str, out: &Path) -> Result<()> {
    let mut cmd = Command::new("lipo");
    cmd.arg(fat).arg("-thin").arg(arch).arg("-output").arg(out);
    run_quiet(&mut cmd, "lipo thin")
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
) -> Result<()> {
    let scheme = scheme.unwrap_or(Consts::DEFAULT_SCHEME).to_owned();
    let simulator = simulator.unwrap_or(Consts::DEFAULT_SIMULATOR).to_owned();

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
        run_build(profile)?;
    }

    let uuid = resolve_simulator_uuid(&simulator)?;
    boot_simulator(&uuid)?;
    open_simulator_app();

    let configuration = match profile {
        crate::BuildProfile::Release => "Release",
        crate::BuildProfile::Debug => "Debug",
    };

    println!("==> Building {scheme} ({configuration}) for simulator {simulator}");
    let destination = format!("platform=iOS Simulator,id={uuid}");
    let mut build = Command::new("xcodebuild");
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
    let status = build.status().context("failed to run xcodebuild")?;
    if !status.success() {
        bail!("xcodebuild failed");
    }

    let app_path = locate_built_app(&demo_dir, &scheme, configuration)?;

    println!("==> Installing {} on simulator", app_path.display());
    let status = Command::new("xcrun")
        .args([
            "simctl",
            "install",
            &uuid,
            app_path.to_str().context(".app path is not UTF-8")?,
        ])
        .status()
        .context("failed to run `xcrun simctl install`")?;
    if !status.success() {
        bail!("simctl install failed");
    }

    println!("==> Launching {}", Consts::DEMO_BUNDLE_ID);
    let mut launch = Command::new("xcrun");
    launch.args(["simctl", "launch"]);
    if debug {
        // Suspends the app on entry so `lldb -p <pid>` (or Zed's
        // CodeLLDB "attach" config) can hook into it before any user
        // code runs.
        launch.arg("--wait-for-debugger");
    }
    launch.args([&uuid, Consts::DEMO_BUNDLE_ID]);
    let output = launch
        .output()
        .context("failed to run `xcrun simctl launch`")?;
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
            println!("    Find it manually: xcrun simctl spawn {uuid} ps -A | grep KitharaDemo");
        }
    }

    Ok(())
}

fn resolve_simulator_uuid(name_or_uuid: &str) -> Result<String> {
    // If the argument already looks like a UUID, accept it as-is.
    if name_or_uuid.len() == 36 && name_or_uuid.chars().filter(|c| *c == '-').count() == 4 {
        return Ok(name_or_uuid.to_owned());
    }
    let output = Command::new("xcrun")
        .args(["simctl", "list", "devices", "available"])
        .output()
        .context("failed to run `xcrun simctl list devices available`")?;
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
    bail!("simulator '{name_or_uuid}' not found in `xcrun simctl list`")
}

fn boot_simulator(uuid: &str) -> Result<()> {
    // `simctl boot` is a no-op (and exits 149 / "Unable to boot") when
    // the device is already booted; treat that as success.
    let status = Command::new("xcrun")
        .args(["simctl", "boot", uuid])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("failed to run `xcrun simctl boot`")?;
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
