use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};
use cargo_metadata::MetadataCommand;

use crate::{
    BuildProfile,
    util::{check_rust_target, check_tool},
};

/// Module constants for `cargo xtask android run`. Grouped per the
/// `style.multiple-private-module-consts` lint.
struct Consts;
impl Consts {
    /// AVD picked when `--avd` is omitted. Kept here so `cargo xtask
    /// android run` works out of the box on the team's typical setup;
    /// the developer overrides it on the CLI when running a different
    /// image.
    const DEFAULT_AVD: &'static str = "Pixel_6";
    /// Application id of the demo APK.
    const DEMO_PACKAGE: &'static str = "com.kithara.example";
    /// Activity component the demo APK surfaces.
    const DEMO_ACTIVITY: &'static str = "com.kithara.example.MainActivity";
}

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
}

pub(crate) fn run(cmd: AndroidCommand) -> Result<()> {
    match cmd {
        AndroidCommand::Build { profile } => run_build(profile),
        AndroidCommand::Aar => run_aar(),
        AndroidCommand::Run {
            profile,
            avd,
            debug,
            skip_build,
        } => run_app(profile, avd.as_deref(), debug, skip_build),
    }
}

fn recreate_dir(path: &Path) -> Result<()> {
    if path.exists() {
        fs::remove_dir_all(path).with_context(|| format!("remove {}", path.display()))?;
    }
    fs::create_dir_all(path).with_context(|| format!("create_dir_all {}", path.display()))?;
    Ok(())
}

pub(crate) fn run_build(profile: BuildProfile) -> Result<()> {
    const ANDROID_API_LEVEL: &str = "26";
    const RUST_TARGETS: &[(&str, &str)] = &[
        ("aarch64-linux-android", "arm64-v8a"),
        ("x86_64-linux-android", "x86_64"),
    ];

    check_tool("cargo", &["ndk", "--help"], "cargo install cargo-ndk")?;
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
    let crate_dir = root.join("crates/kithara-ffi");
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
        "uniffi,android"
    } else {
        "uniffi,android,dev,test"
    };

    let mut cmd = Command::new("cargo");
    cmd.arg("ndk")
        .arg("-P")
        .arg(ANDROID_API_LEVEL)
        .args(&ndk_targets)
        .arg("-o")
        .arg(&jni_dir)
        // Device build: drop default features so `symphonia` is absent —
        // the Android MediaCodec backend is the sole decoder on-device.
        .args([
            "build",
            "-p",
            "kithara-ffi",
            "--no-default-features",
            "--features",
            features,
        ]);

    if matches!(profile, BuildProfile::Release) {
        cmd.arg("--release");
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

    println!("==> Done!");
    println!("==> JNI libs: {}", jni_dir.display());
    println!("==> Kotlin bindings: {}", kotlin_dir.display());

    Ok(())
}

fn run_aar() -> Result<()> {
    run_build(BuildProfile::Release)?;

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
    let aars = [output.join("kithara.aar"), output.join("rust-tls.aar")];
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

fn run_app(profile: BuildProfile, avd: Option<&str>, debug: bool, skip_build: bool) -> Result<()> {
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
        run_build(profile)?;
    }

    ensure_emulator_running(&adb, &emulator, avd.unwrap_or(Consts::DEFAULT_AVD))?;

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

    println!(
        "==> Launching {}/{}",
        Consts::DEMO_PACKAGE,
        Consts::DEMO_ACTIVITY
    );
    let mut cmd = Command::new(&adb);
    cmd.args(["shell", "am", "start"]);
    if debug {
        // `-D` suspends the launched process so a JDWP-aware debugger
        // (Android Studio, Zed via kotlin-debug-adapter) can attach.
        cmd.arg("-D");
    }
    cmd.args([
        "-n",
        &format!("{}/{}", Consts::DEMO_PACKAGE, Consts::DEMO_ACTIVITY),
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
fn ensure_emulator_running(adb: &Path, emulator: &Path, avd_name: &str) -> Result<()> {
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
    wait_for_boot_complete(adb)?;
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

fn wait_for_boot_complete(adb: &Path) -> Result<()> {
    use std::{thread, time::Duration};

    const MAX_ATTEMPTS: u32 = 120;
    const POLL_INTERVAL: Duration = Duration::from_secs(1);

    for _ in 0..MAX_ATTEMPTS {
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
        thread::sleep(POLL_INTERVAL);
    }
    bail!("device did not finish booting within {MAX_ATTEMPTS} seconds");
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
