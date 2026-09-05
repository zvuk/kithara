use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use cargo_metadata::MetadataCommand;
use kithara_devtools::{
    Ctx,
    common::tools::ToolsConfig,
    util::{check_rust_target, check_tool},
};

use crate::{
    BuildProfile,
    config::{AndroidConfig, KitharaExt},
};

#[derive(Clone, Debug, clap::Subcommand)]
pub(crate) enum AndroidCommand {
    /// Build Android shared libraries and Kotlin bindings.
    Build {
        /// Build profile.
        #[arg(long, default_value_t = crate::BuildProfile::Debug)]
        profile: BuildProfile,
    },
    /// Build release JNI/Kotlin bindings and export stable release AAR files.
    Aar,
    /// Boot an emulator (if needed), install the demo APK, and launch it.
    ///
    /// Pass `--debug` to start the activity with `am start -D`, which
    /// suspends the process at launch — Zed (or any JDWP-aware
    /// debugger) can then attach via `adb forward jdwp:<pid>`.
    Run {
        /// Build profile for the underlying Rust JNI libs.
        #[arg(long, default_value_t = crate::BuildProfile::Debug)]
        profile: BuildProfile,
        /// AVD name to boot (must already exist in `avdmanager`).
        #[arg(long)]
        avd: Option<String>,
        /// Suspend the process on launch so a debugger can attach.
        #[arg(long)]
        debug: bool,
        /// Skip the JNI/Kotlin rebuild (use the cached `android/lib/build`).
        #[arg(long)]
        skip_build: bool,
    },
    /// Boot an emulator (if needed) and run the instrumented tests on it.
    Test {
        /// Build profile for the underlying Rust JNI libs.
        #[arg(long, default_value_t = crate::BuildProfile::Debug)]
        profile: BuildProfile,
        /// AVD name to boot (must already exist in `avdmanager`).
        #[arg(long)]
        avd: Option<String>,
        /// Skip the JNI/Kotlin rebuild (use the cached `android/lib/build`).
        #[arg(long)]
        skip_build: bool,
    },
}

pub(crate) fn run(cmd: AndroidCommand, ctx: &Ctx) -> Result<()> {
    let ext = KitharaExt::from_ctx(ctx)?;
    let tools = &ctx.config.tools;
    match cmd {
        AndroidCommand::Build { profile } => run_build(profile, &ext.android, tools),
        AndroidCommand::Aar => run_aar(&ext.android, tools),
        AndroidCommand::Run {
            profile,
            avd,
            debug,
            skip_build,
        } => run_app(
            profile,
            avd.as_deref(),
            debug,
            skip_build,
            &ext.android,
            tools,
        ),
        AndroidCommand::Test {
            profile,
            avd,
            skip_build,
        } => run_tests(profile, avd.as_deref(), skip_build, &ext.android, tools),
    }
}

/// Whether the generator left any Kotlin under this root. It writes into the
/// package path rather than the output directory itself — `kotlin/com/kithara/
/// ffi/kithara_ffi.kt` — so a check that reads only the top level calls a
/// successful run empty.
fn has_kotlin_source(path: &Path) -> Result<bool> {
    let entries = fs::read_dir(path).with_context(|| format!("read_dir {}", path.display()))?;
    for entry in entries {
        let entry = entry.with_context(|| format!("read_dir {}", path.display()))?;
        let candidate = entry.path();
        let found = if candidate.is_dir() {
            has_kotlin_source(&candidate)?
        } else {
            candidate.extension().is_some_and(|kind| kind == "kt")
        };
        if found {
            return Ok(true);
        }
    }
    Ok(false)
}

fn recreate_dir(path: &Path) -> Result<()> {
    if path.exists() {
        fs::remove_dir_all(path).with_context(|| format!("remove {}", path.display()))?;
    }
    fs::create_dir_all(path).with_context(|| format!("create_dir_all {}", path.display()))?;
    Ok(())
}

pub(crate) fn run_build(
    profile: BuildProfile,
    android: &AndroidConfig,
    tools: &ToolsConfig,
) -> Result<()> {
    const RUST_TARGETS: &[(&str, &str)] = &[
        ("aarch64-linux-android", "arm64-v8a"),
        ("x86_64-linux-android", "x86_64"),
    ];

    check_tool(
        "cargo",
        &["ndk", "--help"],
        tools.install_hint("cargo-ndk", "cargo install cargo-ndk"),
    )?;
    check_tool("rustup", &["--version"], "https://rustup.rs")?;

    for (target, _) in RUST_TARGETS {
        if !check_rust_target(target)? {
            bail!("Rust target '{target}' is not installed. Run: rustup target add {target}");
        }
    }

    let metadata = MetadataCommand::new()
        .exec()
        .context("failed to read cargo metadata")?;
    let root = metadata.workspace_root.as_std_path();
    let ffi_crate = require_android_str(&android.ffi_crate, "ffi_crate")?;
    let api_level = require_android_str(&android.api_level, "api_level")?;
    let crate_dir = root.join("crates").join(ffi_crate);
    let jni_dir = root.join("android/lib/build/generated/jniLibs");
    let kotlin_dir = root.join("android/lib/build/generated/uniffi/kotlin");

    recreate_dir(&jni_dir)?;
    recreate_dir(&kotlin_dir)?;

    println!("==> Building Android shared libraries");

    let ndk_targets: Vec<&str> = RUST_TARGETS
        .iter()
        .flat_map(|(_, abi)| ["-t", abi])
        .collect();

    let features: &str = if matches!(profile, BuildProfile::Release) {
        "uniffi,android,stretch-signalsmith"
    } else {
        "uniffi,android,dev,test,stretch-signalsmith"
    };

    let mut cmd = Command::new("cargo");
    cmd.arg("ndk")
        .arg("-P")
        .arg(api_level)
        .args(&ndk_targets)
        .arg("-o")
        .arg(&jni_dir)
        // Device build: drop default features so `symphonia` is absent —
        // the Android MediaCodec backend is the sole decoder on-device.
        .args([
            "build",
            "-p",
            ffi_crate,
            "--no-default-features",
            "--features",
            features,
        ]);

    if matches!(profile, BuildProfile::Release) {
        // `uniffi-bindgen --library` reads the interface out of the static
        // symbol table, and the release profile strips it. The dynamic table
        // survives, so the library still loads and still exports every entry
        // point — the generator simply finds no components, writes no Kotlin,
        // and exits successfully, leaving the Gradle compile to fail on every
        // import of the bindings. Keep the names through this build; Gradle
        // strips the library again on its way into the AAR.
        cmd.args([
            "--release",
            "--config",
            "profile.release.strip=\"debuginfo\"",
        ]);
    }

    cmd.current_dir(root);

    let status = cmd.status().context("failed to run cargo ndk")?;
    if !status.success() {
        bail!("cargo ndk failed");
    }

    let lib_path = jni_dir.join("arm64-v8a/libkithara_ffi.so");
    if !lib_path.exists() {
        bail!("compiled library not found at {}", lib_path.display());
    }

    copy_cxx_runtime(&jni_dir, RUST_TARGETS)?;

    println!("==> Generating Kotlin bindings");

    let mut cmd = Command::new("cargo");
    cmd.args([
        "run",
        "--bin",
        "uniffi-bindgen",
        "--features",
        // symphonia gives the host bindgen build a DecoderBackend
        // variant (the android MediaCodec variant is target_os-gated
        // and absent when compiling the bindgen bin for the host).
        "uniffi-bindgen-cli,symphonia",
    ]);
    if matches!(profile, BuildProfile::Release) {
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
    // A library the generator cannot read is not an error to it: it finds no
    // components and exits successfully, and the miss only surfaces later as
    // an unresolved import in Kotlin.
    if !has_kotlin_source(&kotlin_dir)? {
        bail!(
            "uniffi-bindgen wrote no Kotlin into {}; {} carries no readable interface metadata",
            kotlin_dir.display(),
            lib_path.display()
        );
    }

    println!("==> Done!");
    println!("==> JNI libs: {}", jni_dir.display());
    println!("==> Kotlin bindings: {}", kotlin_dir.display());

    Ok(())
}

fn run_aar(android: &AndroidConfig, tools: &ToolsConfig) -> Result<()> {
    run_build(BuildProfile::Release, android, tools)?;

    let metadata = MetadataCommand::new()
        .exec()
        .context("failed to read cargo metadata")?;
    let workspace_root = metadata.workspace_root.as_std_path().to_path_buf();
    let android_root = workspace_root.join("android");
    let gradlew = android_root.join("gradlew");
    if !gradlew.exists() {
        bail!("gradlew not found at {}", gradlew.display());
    }

    println!("==> Exporting release AARs");
    let status = Command::new(&gradlew)
        .args([
            ":lib:exportReleaseAars",
            "-Pkithara.release=true",
            "-x",
            "generateKitharaFfi",
        ])
        .current_dir(&android_root)
        .status()
        .context("failed to run Gradle exportReleaseAars")?;
    if !status.success() {
        bail!("Gradle exportReleaseAars failed");
    }

    let output = android_root.join("lib/build/outputs/aar");
    let aars: Vec<PathBuf> = android.aars.iter().map(|name| output.join(name)).collect();
    for aar in &aars {
        if !aar.is_file() {
            bail!("expected AAR was not produced: {}", aar.display());
        }
    }

    println!("==> AARs:");
    for aar in &aars {
        println!("    {}", aar.display());
    }
    Ok(())
}

/// Everything both the demo launch and the instrumented tests need: a booted
/// device to talk to and a Gradle wrapper to drive it.
struct Device {
    adb: PathBuf,
    android_root: PathBuf,
    gradlew: PathBuf,
}

/// Whether the emulator draws. Launching the demo is the case where someone is
/// watching; the instrumented tests read their verdict through `adb`, and the
/// machine that runs them has no display to draw on.
#[derive(Clone, Copy)]
enum Screen {
    Windowed,
    Headless,
}

impl Screen {
    const fn args(self) -> &'static [&'static str] {
        match self {
            Self::Windowed => &[],
            Self::Headless => &["-no-window"],
        }
    }
}

fn prepare_device(
    profile: BuildProfile,
    avd: Option<&str>,
    skip_build: bool,
    screen: Screen,
    android: &AndroidConfig,
    tools: &ToolsConfig,
) -> Result<Device> {
    let sdk_root = android_sdk_root()?;
    let adb = sdk_root.join("platform-tools/adb");
    let emulator = sdk_root.join("emulator/emulator");
    if !adb.exists() {
        bail!("adb not found at {}", adb.display());
    }

    let metadata = MetadataCommand::new()
        .exec()
        .context("failed to read cargo metadata")?;
    let workspace_root = metadata.workspace_root.as_std_path().to_path_buf();
    let android_root = workspace_root.join("android");
    let gradlew = android_root.join("gradlew");
    if !gradlew.exists() {
        bail!("gradlew not found at {}", gradlew.display());
    }

    if !skip_build {
        run_build(profile, android, tools)?;
    }

    let avd_name = match avd {
        Some(avd) => avd,
        None => require_android_str(&android.default_avd, "default_avd")?,
    };
    ensure_emulator_running(&adb, &emulator, avd_name, screen, android)?;

    Ok(Device {
        adb,
        android_root,
        gradlew,
    })
}

/// Run the instrumented tests, then put the emulator down whether they passed
/// or not: a CI machine that leaves one running has one fewer job's worth of
/// memory for the next job.
fn run_tests(
    profile: BuildProfile,
    avd: Option<&str>,
    skip_build: bool,
    android: &AndroidConfig,
    tools: &ToolsConfig,
) -> Result<()> {
    let device = prepare_device(profile, avd, skip_build, Screen::Headless, android, tools)?;

    println!("==> Running instrumented tests via gradle");
    let tests = Command::new(&device.gradlew)
        .args([
            ":lib:connectedDebugAndroidTest",
            "-x",
            "generateKitharaFfi",
            "--no-daemon",
        ])
        .current_dir(&device.android_root)
        .status()
        .with_context(|| format!("failed to run {}", device.gradlew.display()))
        .and_then(|status| {
            status
                .success()
                .then_some(())
                .context("gradle connected tests failed")
        });

    let shutdown = Command::new(&device.adb)
        .args(["emu", "kill"])
        .status()
        .context("failed to invoke `adb emu kill`")
        .and_then(|status| {
            status
                .success()
                .then_some(())
                .context("adb emu kill failed")
        });

    tests.and(shutdown)
}

fn run_app(
    profile: BuildProfile,
    avd: Option<&str>,
    debug: bool,
    skip_build: bool,
    android: &AndroidConfig,
    tools: &ToolsConfig,
) -> Result<()> {
    let Device {
        adb,
        android_root,
        gradlew,
    } = prepare_device(profile, avd, skip_build, Screen::Windowed, android, tools)?;

    println!("==> Installing demo APK via gradle");
    let gradle_task = match profile {
        BuildProfile::Release => ":example:installRelease",
        BuildProfile::Debug => ":example:installDebug",
    };
    let status = Command::new(&gradlew)
        .arg(gradle_task)
        .current_dir(&android_root)
        .status()
        .with_context(|| format!("failed to run {} {}", gradlew.display(), gradle_task))?;
    if !status.success() {
        bail!("gradle install task failed: {gradle_task}");
    }

    let package = require_android_str(&android.demo_package, "demo_package")?;
    let activity = require_android_str(&android.demo_activity, "demo_activity")?;
    println!("==> Launching {package}/{activity}");
    let mut cmd = Command::new(&adb);
    cmd.args(["shell", "am", "start"]);
    if debug {
        // `-D` suspends the launched process so a JDWP-aware debugger
        // (Android Studio, Zed via kotlin-debug-adapter) can attach.
        cmd.arg("-D");
    }
    cmd.args([
        "-n",
        &format!("{package}/{activity}"),
        "-a",
        "android.intent.action.MAIN",
        "-c",
        "android.intent.category.LAUNCHER",
    ]);
    let status = cmd
        .status()
        .context("failed to invoke `adb shell am start`")?;
    if !status.success() {
        bail!("adb shell am start failed");
    }

    if debug {
        print_jdwp_attach_hint(&adb);
    }

    Ok(())
}

/// Put the NDK's C++ runtime beside the library that needs it.
///
/// `cargo ndk` writes the library it built and nothing else, and the stretch
/// backend links the C++ standard library. On the device that showed up as
/// `java.lang.UnsatisfiedLinkError: dlopen failed: library "libc++_shared.so"
/// not found` — the connected suite installed, started, and could not load a
/// single test.
fn copy_cxx_runtime(jni_dir: &Path, targets: &[(&str, &str)]) -> Result<()> {
    let sysroot = ndk_prebuilt()?.join("sysroot/usr/lib");
    for (target, abi) in targets {
        let source = sysroot.join(target).join("libc++_shared.so");
        if !source.is_file() {
            bail!("NDK C++ runtime not found at {}", source.display());
        }
        let destination = jni_dir.join(abi).join("libc++_shared.so");
        fs::copy(&source, &destination).with_context(|| {
            format!("copying {} to {}", source.display(), destination.display())
        })?;
    }
    println!("==> Bundled the NDK C++ runtime");
    Ok(())
}

/// The toolchain inside the NDK. An NDK is downloaded for one machine and
/// carries one, under a name that describes that machine rather than the
/// architecture it builds for — Apple silicon reads `darwin-x86_64` as Intel
/// does. Reading the directory rather than naming it keeps the answer right on
/// a machine nobody had in mind.
fn ndk_prebuilt() -> Result<PathBuf> {
    let prebuilt = ndk_root()?.join("toolchains/llvm/prebuilt");
    let mut hosts = fs::read_dir(&prebuilt)
        .with_context(|| format!("reading the NDK toolchains in {}", prebuilt.display()))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.is_dir());
    let host = hosts
        .next()
        .with_context(|| format!("the NDK at {} carries no toolchain", prebuilt.display()))?;
    if let Some(extra) = hosts.next() {
        bail!(
            "the NDK at {} carries more than one toolchain: {} and {}",
            prebuilt.display(),
            host.display(),
            extra.display()
        );
    }
    Ok(host)
}

fn ndk_root() -> Result<PathBuf> {
    for name in ["ANDROID_NDK_HOME", "ANDROID_NDK_ROOT", "NDK_HOME"] {
        if let Ok(value) = env::var(name) {
            return Ok(PathBuf::from(value));
        }
    }
    let ndk = android_sdk_root()?.join("ndk");
    let mut versions: Vec<PathBuf> = fs::read_dir(&ndk)
        .with_context(|| format!("reading the installed NDK versions in {}", ndk.display()))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.is_dir())
        .collect();
    versions.sort();
    versions
        .pop()
        .with_context(|| format!("no NDK installed under {}", ndk.display()))
}

fn android_sdk_root() -> Result<PathBuf> {
    if let Ok(value) = env::var("ANDROID_HOME") {
        return Ok(PathBuf::from(value));
    }
    if let Ok(value) = env::var("ANDROID_SDK_ROOT") {
        return Ok(PathBuf::from(value));
    }
    if let Ok(home) = env::var("HOME") {
        let candidate = PathBuf::from(home).join("Library/Android/sdk");
        if candidate.exists() {
            return Ok(candidate);
        }
    }
    bail!("ANDROID_HOME / ANDROID_SDK_ROOT not set and ~/Library/Android/sdk does not exist")
}

/// Make sure at least one device is online; if none is, boot the AVD in
/// the background and wait for it to finish booting.
fn ensure_emulator_running(
    adb: &Path,
    emulator: &Path,
    avd_name: &str,
    screen: Screen,
    android: &AndroidConfig,
) -> Result<()> {
    if has_online_device(adb)? {
        println!("==> Using already-connected device");
        return Ok(());
    }

    if !emulator.exists() {
        bail!(
            "no device connected and emulator binary missing at {}",
            emulator.display()
        );
    }

    println!("==> Booting AVD '{avd_name}' in the background");
    Command::new(emulator)
        .args(["-avd", avd_name])
        .args(screen.args())
        .spawn()
        .with_context(|| format!("failed to spawn emulator -avd {avd_name}"))?;

    println!("==> Waiting for device to come online");
    let status = Command::new(adb)
        .arg("wait-for-device")
        .status()
        .context("failed to invoke `adb wait-for-device`")?;
    if !status.success() {
        bail!("adb wait-for-device failed");
    }

    // `wait-for-device` returns as soon as adb sees the device, which
    // is well before the system finishes booting; poll
    // `sys.boot_completed` so the install step doesn't race the
    // package manager.
    wait_for_boot_complete(adb, android)?;
    Ok(())
}

fn has_online_device(adb: &Path) -> Result<bool> {
    let output = Command::new(adb)
        .arg("devices")
        .output()
        .context("failed to run `adb devices`")?;
    if !output.status.success() {
        bail!("adb devices failed");
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout
        .lines()
        .skip(1)
        .any(|line| line.ends_with("\tdevice")))
}

fn wait_for_boot_complete(adb: &Path, android: &AndroidConfig) -> Result<()> {
    let max_attempts = android.boot_wait_attempts.context(
        "ext.android.boot_wait_attempts is not set; fill in the [ext.android] section of .config/xtask.toml",
    )?;
    let poll_interval = Duration::from_secs(android.boot_poll_interval_secs.context(
        "ext.android.boot_poll_interval_secs is not set; fill in the [ext.android] section of .config/xtask.toml",
    )?);

    for _ in 0..max_attempts {
        let output = Command::new(adb)
            .args(["shell", "getprop", "sys.boot_completed"])
            .output();
        if let Ok(output) = output
            && output.status.success()
        {
            let value = String::from_utf8_lossy(&output.stdout);
            if value.trim() == "1" {
                println!("==> Device boot complete");
                return Ok(());
            }
        }
        thread::sleep(poll_interval);
    }
    let timeout_secs = u64::from(max_attempts).saturating_mul(poll_interval.as_secs());
    bail!("device did not finish booting within {timeout_secs} seconds");
}

fn require_android_str<'a>(value: &'a str, key: &str) -> Result<&'a str> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        bail!(
            "ext.android.{key} is not set; fill in the [ext.android] section of .config/xtask.toml"
        );
    }
    Ok(trimmed)
}

/// Print attach instructions after `am start -D`. Failures here are
/// non-fatal: the app is already running suspended.
fn print_jdwp_attach_hint(adb: &Path) {
    let pid = Command::new(adb).arg("jdwp").output().ok().and_then(|out| {
        String::from_utf8(out.stdout)
            .ok()?
            .lines()
            .map(str::trim)
            .rfind(|line| !line.is_empty())
            .map(str::to_owned)
    });

    println!();
    println!("==> App is suspended waiting for a debugger.");
    if let Some(pid) = pid {
        println!("    Forward the JDWP socket:    adb forward tcp:8700 jdwp:{pid}");
    } else {
        println!("    Discover JDWP pids:         adb jdwp");
        println!("    Forward the JDWP socket:    adb forward tcp:8700 jdwp:<pid>");
    }
    println!("    Then attach your debugger to localhost:8700.");
}
