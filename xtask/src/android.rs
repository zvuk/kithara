mod composition;
mod device;
mod evidence;
mod native;
mod results;

use std::{
    collections::BTreeMap,
    env,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::{Command, ExitStatus},
};

use anyhow::{Context, Result, bail};
use cargo_metadata::MetadataCommand;
use kithara_devtools::{
    Ctx,
    common::{project::ProjectConfig, tools::ToolsConfig},
    lock::FileLock,
    util::{check_rust_target, check_tool},
};

use self::device::{Request, Reverse, Screen, Selected};
use crate::{
    BuildProfile, child,
    ci::process::Process,
    config::{AndroidConfig, KitharaExt},
    test_server::{Port, TestServer},
};

#[derive(Clone, Debug, clap::Subcommand)]
pub(crate) enum AndroidCommand {
    #[command(hide = true)]
    NativeLink {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<OsString>,
    },
    #[command(hide = true)]
    NativeRunner {
        #[arg(long)]
        session: PathBuf,
        binary: PathBuf,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Build Android shared libraries and Kotlin bindings.
    Build {
        /// Build profile.
        #[arg(long, default_value_t = crate::BuildProfile::Debug)]
        profile: BuildProfile,
    },
    /// Run Clippy over the Android device build.
    Clippy,
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
    /// Boot an emulator (if needed) and run the instrumented tests on it
    /// against a hermetic fixture server this command owns.
    Test {
        /// Build profile for the underlying Rust JNI libs.
        #[arg(long, default_value_t = crate::BuildProfile::Debug)]
        profile: BuildProfile,
        /// AVD name to boot (must already exist in `avdmanager`).
        #[arg(long, conflicts_with = "serial")]
        avd: Option<String>,
        /// Serial of an already-online device to run on.
        #[arg(long)]
        serial: Option<String>,
        /// Skip the JNI/Kotlin rebuild (use the cached `android/lib/build`).
        #[arg(long)]
        skip_build: bool,
    },
}

/// Cargo and nextest invoke these from the crate they are building, where the
/// repository root stays out of reach.
pub(crate) fn run_native_shim(cmd: &AndroidCommand) -> Option<Result<()>> {
    match cmd {
        AndroidCommand::NativeLink { args } => Some(native::link(args)),
        AndroidCommand::NativeRunner {
            session,
            binary,
            args,
        } => Some(run_native_binary(session, binary, args)),
        _ => None,
    }
}

fn run_native_binary(session: &Path, binary: &Path, args: &[String]) -> Result<()> {
    let code = native::run_binary(session, binary, args)?;
    if code != 0 {
        return Err(kithara_devtools::verdict::ChildFailure::inherited(
            "Android libtest".to_owned(),
            Some(code),
        ));
    }
    Ok(())
}

pub(crate) fn run(cmd: AndroidCommand, ctx: &Ctx) -> Result<()> {
    let ext = KitharaExt::from_ctx(ctx)?;
    let tools = &ctx.config.tools;
    match cmd {
        AndroidCommand::NativeLink { args } => native::link(&args),
        AndroidCommand::NativeRunner {
            session,
            binary,
            args,
        } => run_native_binary(&session, &binary, &args),
        AndroidCommand::Build { profile } => run_build(profile, &ext.android, tools),
        AndroidCommand::Clippy => run_clippy(&ctx.root, &ext.android, tools),
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
            serial,
            skip_build,
        } => run_tests(
            &ctx.root,
            &ctx.config,
            profile,
            request(avd.as_deref(), serial.as_deref()),
            skip_build,
            &ext.android,
        ),
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

/// Rust targets the device build compiles, each with its Android ABI name.
const RUST_TARGETS: &[(&str, &str)] = &[
    ("aarch64-linux-android", "arm64-v8a"),
    ("x86_64-linux-android", "x86_64"),
];

/// Features the FFI crate is compiled with on-device. Defaults stay off so
/// `symphonia` is absent: `MediaCodec` is the sole decoder there.
const fn device_features(profile: BuildProfile) -> &'static str {
    match profile {
        BuildProfile::Release => {
            "kithara-ffi/uniffi,kithara-ffi/android,kithara-ffi/stretch-signalsmith"
        }
        BuildProfile::Debug => {
            "kithara-ffi/uniffi,kithara-ffi/android,kithara-ffi/dev,kithara-ffi/test,kithara-ffi/stretch-signalsmith"
        }
    }
}

fn check_ndk_toolchain(tools: &ToolsConfig) -> Result<()> {
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
    Ok(())
}

/// `cargo ndk` primed for every device ABI. Native build scripts read the NDK
/// out of the environment.
fn cargo_ndk(api_level: &str) -> Result<Command> {
    let mut cmd = Command::new("cargo");
    cmd.env("ANDROID_NDK_HOME", ndk_root()?);
    cmd.arg("ndk").arg("-P").arg(api_level);
    for (_, abi) in RUST_TARGETS {
        cmd.args(["-t", abi]);
    }
    Ok(cmd)
}

/// Clippy over the Android backends. The host lint chain compiles for the
/// host, where every `target_os = "android"` item is configured out and unseen.
fn run_clippy(root: &Path, android: &AndroidConfig, tools: &ToolsConfig) -> Result<()> {
    const CLIPPY_PACKAGES: &[&str] = &["kithara-ffi", "kithara-decode", "kithara-audio"];

    check_ndk_toolchain(tools)?;
    let api_level = require_android_str(&android.api_level, "api_level")?;

    println!("==> Linting the Android backends");

    let features = device_features(BuildProfile::Release);

    let mut cmd = cargo_ndk(api_level)?;
    cmd.arg("clippy");
    for package in CLIPPY_PACKAGES {
        cmd.args(["-p", package]);
    }
    cmd.args(["--no-default-features", "--features", features]);
    cmd.args(["--", "-D", "warnings"]);
    cmd.current_dir(root);

    let status = cmd.status().context("failed to run cargo ndk clippy")?;
    if !status.success() {
        bail!("cargo ndk clippy failed");
    }
    Ok(())
}

pub(crate) fn run_build(
    profile: BuildProfile,
    android: &AndroidConfig,
    tools: &ToolsConfig,
) -> Result<()> {
    check_ndk_toolchain(tools)?;

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

    let mut cmd = cargo_ndk(api_level)?;
    cmd.arg("-o").arg(&jni_dir).args([
        "build",
        "-p",
        ffi_crate,
        "--no-default-features",
        "--features",
        device_features(profile),
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

/// Resolved before the run takes anything, so a failure while preparing is
/// still recorded somewhere.
struct Layout {
    workspace_root: PathBuf,
    android_root: PathBuf,
    gradlew: PathBuf,
    adb: PathBuf,
    emulator: PathBuf,
}

impl Layout {
    fn resolve() -> Result<Self> {
        let metadata = MetadataCommand::new()
            .exec()
            .context("failed to read cargo metadata")?;
        Self::at(metadata.workspace_root.as_std_path().to_path_buf())
    }

    fn at(workspace_root: PathBuf) -> Result<Self> {
        let sdk_root = android_sdk_root()?;
        let adb = sdk_root.join("platform-tools/adb");
        if !adb.exists() {
            bail!("adb not found at {}", adb.display());
        }
        let android_root = workspace_root.join("android");
        let gradlew = android_root.join("gradlew");
        if !gradlew.exists() {
            bail!("gradlew not found at {}", gradlew.display());
        }

        Ok(Self {
            workspace_root,
            android_root,
            gradlew,
            adb,
            emulator: sdk_root.join("emulator/emulator"),
        })
    }
}

struct Prepared<'a> {
    device: Selected,
    layout: &'a Layout,
}

impl Prepared<'_> {
    /// `ANDROID_SERIAL` is what the Android tooling reads to pick a target, so
    /// a second device on the host cannot receive this run's APK.
    fn gradle(&self) -> Command {
        let mut command = Command::new(&self.layout.gradlew);
        command
            .env("ANDROID_SERIAL", &self.device.serial)
            .current_dir(&self.layout.android_root);
        command
    }
}

fn request<'a>(avd: Option<&'a str>, serial: Option<&'a str>) -> Request<'a> {
    match (serial, avd) {
        (Some(serial), _) => Request::Serial(serial),
        (None, Some(avd)) => Request::Avd(avd),
        (None, None) => Request::Any,
    }
}

fn prepare_device<'a>(
    layout: &'a Layout,
    profile: BuildProfile,
    request: Request<'_>,
    skip_build: bool,
    screen: Screen,
    android: &AndroidConfig,
    tools: &ToolsConfig,
) -> Result<Prepared<'a>> {
    if !skip_build {
        run_build(profile, android, tools)?;
    }

    let device = device::select(
        &layout.adb,
        &layout.emulator,
        request,
        screen,
        android,
        None,
    )?;
    Ok(Prepared { device, layout })
}

/// Run instrumentation and record cleanup on success, failure, and cancellation.
fn run_tests(
    workspace_root: &Path,
    config: &ProjectConfig,
    profile: BuildProfile,
    request: Request<'_>,
    skip_build: bool,
    android: &AndroidConfig,
) -> Result<()> {
    let _run_lease = test_run_lease(workspace_root)?;
    let evidence = evidence::Dir::create(workspace_root)?;
    let report = workspace_root.join("target/android-test/junit.xml");
    if report.exists() {
        fs::remove_file(&report).context("removing previous Android JUnit")?;
    }
    let mut record = evidence::Manifest::open(&evidence, workspace_root, profile)?;
    let cancel = child::Cancel::install()?;

    let mut device = None;
    let mut device_lease = None;
    let mut server = None;
    let mut reverse = None;
    let tests = (|| {
        let layout = record.stage("layout", Layout::at(workspace_root.to_path_buf()))?;
        child::check(Some(&cancel))?;
        if !skip_build {
            let build = child::run(
                Command::new(env::current_exe()?).args([
                    "android",
                    "build",
                    "--profile",
                    &profile.to_string(),
                ]),
                Some(&cancel),
            )
            .and_then(|status| {
                status
                    .success()
                    .then_some(())
                    .context("Android JNI build failed")
            });
            record.stage("jni_build", build)?;
        }
        device = Some(record.stage(
            "device",
            device::select(
                &layout.adb,
                &layout.emulator,
                request,
                Screen::Headless,
                android,
                Some(&cancel),
            ),
        )?);
        let selected = device.as_ref().context("selected device")?;
        device_lease = Some(record.stage("device_lease", selected.lease())?);
        record.device(&layout.workspace_root, selected, &cancel);
        let process = ambient_process(&layout.workspace_root);
        server = Some(record.stage(
            "fixture_server",
            TestServer::start(
                &process,
                Port::Ephemeral,
                &evidence.server_log(),
                Some(&cancel),
            ),
        )?);
        let server = server.as_ref().context("started fixture server")?;
        reverse = Some(record.stage(
            "reverse_mapping",
            Reverse::create(selected, host_port(server.url())?, Some(&cancel)),
        )?);
        let reverse = reverse.as_ref().context("created reverse mapping")?;
        let device_url = format!("http://127.0.0.1:{}", reverse.device_port());
        record.fixture_server(server.url(), &device_url, reverse.device_port());
        record.stage(
            "reverse_probe",
            device::probe_origin(selected, &device_url, Some(&cancel)),
        )?;
        device::control(selected.adb().args(["logcat", "-c"]), Some(&cancel))?;
        println!("==> Running instrumented tests via gradle");
        let tests =
            run_gradle(&layout, selected, &device_url, &cancel, &evidence).and_then(|status| {
                record.gradle_exit(status.code());
                status
                    .success()
                    .then_some(())
                    .context("gradle connected tests failed")
            });
        let tests = record.stage("gradle", tests);
        let instrumentation = evidence.path().join("instrumentation.xml");
        let instrumentation_result = record.stage(
            "instrumentation",
            results::collect(&evidence.results(), &instrumentation),
        );
        if let Ok(cases) = &instrumentation_result {
            record.instrumentation(cases);
        }
        if instrumentation.is_file() {
            results::merge(std::slice::from_ref(&instrumentation), &report)?;
        }
        tests?;
        instrumentation_result?;
        let native = record.stage(
            "rust_prepare",
            native::prepare(workspace_root, config, selected, evidence.path(), &cancel),
        )?;
        let rust = record.stage("rust_tests", native.run(&device_url, &cancel));
        let native_report = evidence.path().join("native/junit.xml");
        if native_report.is_file() {
            results::merge(&[instrumentation, native_report], &report)?;
        }
        rust
    })();
    let tests = record.stage("run", tests);
    if let Some(device) = &device {
        evidence.capture_logcat(device);
    }
    let owned_reverse = reverse.is_some();
    let unmapped = reverse.map_or(Ok(()), Reverse::remove);
    let stopped = server.map_or(Ok(()), TestServer::stop);
    let owned_emulator = device.as_ref().map(Selected::owns_emulator);
    let released = device.map_or(Ok(()), Selected::release);
    record.cleanup(
        &unmapped,
        &stopped,
        &released,
        owned_emulator,
        owned_reverse,
    );
    let written = record.write();
    drop(device_lease);
    tests.and(unmapped).and(stopped).and(released).and(written)
}

fn test_run_lease(root: &Path) -> Result<FileLock> {
    let directory = root.join("target/android-test");
    fs::create_dir_all(&directory)?;
    let file = fs::File::options()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(directory.join("run.lock"))?;
    FileLock::try_exclusive(file).context("another Android test run owns this workspace")
}

/// Waiting on Gradle's exit status would leave a cancelled run holding the
/// device until the whole suite finished.
fn run_gradle(
    layout: &Layout,
    device: &Selected,
    device_url: &str,
    cancel: &child::Cancel,
    evidence: &evidence::Dir,
) -> Result<ExitStatus> {
    let log = fs::File::create(evidence.gradle_log()).context("creating Gradle log")?;
    child::run(
        Command::new(&layout.gradlew)
            .current_dir(&layout.android_root)
            .env("ANDROID_SERIAL", &device.serial)
            .stdout(log.try_clone()?)
            .stderr(log)
            .args([
                ":lib:connectedDebugAndroidTest",
                "-x",
                "generateKitharaFfi",
                "--no-daemon",
                &format!("-Pkithara.testResultsDir={}", evidence.results().display()),
                &format!("-Pkithara.testReportDir={}", evidence.report().display()),
                &format!(
                    "-Pandroid.testInstrumentationRunnerArguments.{}={device_url}",
                    evidence::Manifest::URL_ARGUMENT
                ),
            ]),
        Some(cancel),
    )
}

fn ambient_process(root: &Path) -> Process {
    let vars = env::var_os("CARGO_TARGET_DIR")
        .map(|dir| BTreeMap::from([(OsString::from("CARGO_TARGET_DIR"), dir)]))
        .unwrap_or_default();
    Process::new(root, vars)
}

fn host_port(url: &str) -> Result<u16> {
    url.rsplit_once(':')
        .context("the fixture server URL carries no port")
        .and_then(|(_, port)| {
            port.trim_end_matches('/')
                .parse()
                .with_context(|| format!("`{url}` carries no numeric port"))
        })
}

/// The device is left running: an emulator that shuts down with the command
/// that booted it takes the app off the screen it was launched to appear on.
fn run_app(
    profile: BuildProfile,
    avd: Option<&str>,
    debug: bool,
    skip_build: bool,
    android: &AndroidConfig,
    tools: &ToolsConfig,
) -> Result<()> {
    let layout = Layout::resolve()?;
    let mut prepared = prepare_device(
        &layout,
        profile,
        request(avd, None),
        skip_build,
        Screen::Windowed,
        android,
        tools,
    )?;

    let launched = launch_app(&prepared, profile, debug, android);
    prepared.device.leave_running();
    launched
}

fn launch_app(
    prepared: &Prepared<'_>,
    profile: BuildProfile,
    debug: bool,
    android: &AndroidConfig,
) -> Result<()> {
    println!("==> Installing demo APK via gradle");
    let gradle_task = match profile {
        BuildProfile::Release => ":example:installRelease",
        BuildProfile::Debug => ":example:installDebug",
    };
    let status = prepared
        .gradle()
        .arg(gradle_task)
        .status()
        .with_context(|| {
            format!(
                "failed to run {} {gradle_task}",
                prepared.layout.gradlew.display()
            )
        })?;
    if !status.success() {
        bail!("gradle install task failed: {gradle_task}");
    }

    let package = require_android_str(&android.demo_package, "demo_package")?;
    let activity = require_android_str(&android.demo_activity, "demo_activity")?;
    println!("==> Launching {package}/{activity}");
    let mut cmd = prepared.device.adb();
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
        print_jdwp_attach_hint(&prepared.device);
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
fn print_jdwp_attach_hint(device: &Selected) {
    let pid = device.adb().arg("jdwp").output().ok().and_then(|out| {
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
