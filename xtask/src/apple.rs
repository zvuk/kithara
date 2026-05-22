use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{Context, Result, bail};
use cargo_metadata::MetadataCommand;

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

#[derive(Clone, Debug, clap::Subcommand)]
pub(crate) enum AppleCommand {
    /// Build `XCFramework` for Apple platforms.
    Build {
        /// Build profile.
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
}

pub(crate) fn run(cmd: AppleCommand) -> Result<()> {
    match cmd {
        AppleCommand::Build { profile } => run_build(profile),
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
    }
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
        "backend-uniffi,apple,dev",
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
