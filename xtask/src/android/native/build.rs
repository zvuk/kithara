use std::{
    collections::BTreeMap,
    env, fs,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use kithara_devtools::{
    common::project::ProjectConfig,
    lock::FileLock,
    test::{NextestAction, nextest_command_for_lane},
};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::{Prepared, link, logged, runner::Session, sha256};
use crate::{
    android::{android_sdk_root, device::Selected, ndk_prebuilt, ndk_root},
    child,
    config::KitharaExt,
};

const PACKAGE: &str = "com.kithara.nativetest";

pub(super) fn prepare(
    root: &Path,
    config: &ProjectConfig,
    device: &Selected,
    evidence_dir: &Path,
    cancel: &child::Cancel,
) -> Result<Prepared> {
    let evidence = evidence_dir.join("native");
    fs::create_dir_all(&evidence)?;
    let abi = child::output(
        device
            .adb()
            .args(["shell", "getprop", "ro.product.cpu.abi"]),
        Some(cancel),
        Duration::from_secs(10),
    )?;
    if !abi.status.success() {
        bail!("reading device ABI failed");
    }
    let abi = String::from_utf8(abi.stdout)?.trim().to_owned();
    let target = match abi.as_str() {
        "arm64-v8a" => "aarch64-linux-android",
        "x86_64" => "x86_64-linux-android",
        _ => bail!("Android native test ABI {abi} is not supported"),
    };
    let Configured {
        ndk,
        toolchain,
        cargo_target,
        environment,
        lease: cache_lease,
    } = configure(root, &evidence, &abi, target, cancel)?;
    let android = KitharaExt::load(root)?.android;
    let packages = super::android_product_packages(root, target, &android.ffi_crate)?;
    let extra = super::art_nextest_list_extra(target, &packages);
    let mut command =
        nextest_command_for_lane(config, &android.test_lane, &extra, NextestAction::List)?;
    fs::write(
        evidence.join("package-selection.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "lane": android.test_lane,
            "packages": super::command_packages(&command)?,
        }))?,
    )?;
    command
        .current_dir(root)
        .envs(&environment)
        .env_remove("FFMPEG_DIR")
        .env("RUSTFLAGS", "-C prefer-dynamic")
        .env("CARGO_TARGET_DIR", &cargo_target);
    crate::android::composition::verify(root, target, &command, &evidence)?;
    logged(
        &mut command,
        &evidence.join("binaries.json"),
        &evidence.join("build.log"),
        cancel,
    )?;
    let libraries = evidence.join("jniLibs").join(&abi);
    fs::create_dir_all(&libraries)?;
    let binaries = package_libraries(
        &evidence.join("binaries.json"),
        &libraries,
        &toolchain,
        cancel,
    )?;
    retain_packaged_binaries(&evidence.join("binaries.json"), &binaries)?;
    fs::copy(
        toolchain
            .join("sysroot/usr/lib")
            .join(target)
            .join("libc++_shared.so"),
        libraries.join("libc++_shared.so"),
    )?;
    stage_shared(&binaries, target, &libraries, &toolchain, cancel)?;
    refuse_oversized_jni_libs(&libraries)?;
    let home = install(root, &evidence, &ndk, device, cancel)?;
    let directory = format!(
        "{}/files/{}",
        home,
        evidence_dir
            .file_name()
            .context("evidence run directory")?
            .to_string_lossy()
    );
    let session = Session {
        adb: android_sdk_root()?.join("platform-tools/adb"),
        serial: device.serial.clone(),
        package: PACKAGE.into(),
        directory,
        evidence: evidence.join("invocations"),
        environment: BTreeMap::new(),
        binaries,
    };
    fs::create_dir_all(&session.evidence)?;
    if let Err(error) = session.control(
        &["run-as", PACKAGE, "mkdir", "-p", &session.directory],
        Some(cancel),
    ) {
        return match session.control(&["run-as", PACKAGE, "rm", "-rf", &session.directory], None) {
            Ok(_) => Err(error),
            Err(cleanup) => Err(error.context(cleanup)),
        };
    }
    Ok(Prepared {
        root: root.to_owned(),
        evidence,
        target: target.into(),
        cargo_target,
        environment,
        session,
        _cache_lease: cache_lease,
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
struct BinaryList {
    rust_binaries: BTreeMap<String, Binary>,
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
struct Binary {
    binary_path: PathBuf,
    build_platform: String,
}

pub(crate) fn is_shared_graph_library(name: &str) -> bool {
    name.starts_with("libkithara_test_dylib")
}

pub(crate) fn oversized_test_libraries(entries: &[(String, u64)]) -> Vec<(String, u64)> {
    const MAX_TEST_LIBRARY_BYTES: u64 = 64 * 1024 * 1024;
    entries
        .iter()
        .filter(|(name, size)| {
            name.ends_with(".so")
                && !is_shared_graph_library(name)
                && *size > MAX_TEST_LIBRARY_BYTES
        })
        .cloned()
        .collect()
}

fn refuse_oversized_jni_libs(libraries: &Path) -> Result<()> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(libraries)
        .with_context(|| format!("reading {} before APK packaging", libraries.display()))?
    {
        let entry = entry?;
        entries.push((
            entry.file_name().to_string_lossy().into_owned(),
            entry.metadata()?.len(),
        ));
    }
    refuse_oversized_staging(&entries)
}

/// The per-image limit catches a test image that missed the shared graph. The
/// total limit holds install time under the 120 s it is given, at a measured
/// 7.9 s per gibibyte.
fn refuse_oversized_staging(entries: &[(String, u64)]) -> Result<()> {
    const MAX_JNI_LIBS_BYTES: u64 = 2 * 1024 * 1024 * 1024;
    let fat = oversized_test_libraries(entries);
    if !fat.is_empty() {
        let detail = fat
            .iter()
            .map(|(name, size)| format!("{name}={size}"))
            .collect::<Vec<_>>()
            .join(", ");
        bail!(
            "Android test libraries still statically link the product graph ({detail}); refuse APK packaging"
        );
    }
    let total: u64 = entries.iter().map(|(_, size)| size).sum();
    if total > MAX_JNI_LIBS_BYTES {
        bail!("jniLibs is {total} bytes; refuse APK packaging above {MAX_JNI_LIBS_BYTES}");
    }
    Ok(())
}

/// `-C prefer-dynamic` leaves every test image with an undefined reference to
/// the standard library and to the crate carrying the shared graph. Android
/// resolves both out of the directory the installer populates.
fn stage_shared(
    binaries: &BTreeMap<PathBuf, String>,
    target: &str,
    libraries: &Path,
    toolchain: &Path,
    cancel: &child::Cancel,
) -> Result<()> {
    let sysroot = String::from_utf8(
        Command::new("rustc")
            .args(["--print", "sysroot"])
            .output()?
            .stdout,
    )?;
    let mut sources = vec![
        PathBuf::from(sysroot.trim())
            .join("lib/rustlib")
            .join(target)
            .join("lib"),
    ];
    let built = binaries
        .keys()
        .next()
        .and_then(|binary| binary.parent())
        .context("locating the directory nextest built the test images in")?;
    sources.push(built.to_path_buf());
    for source in sources {
        for entry in fs::read_dir(&source)
            .with_context(|| format!("reading {} for shared libraries", source.display()))?
        {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !name.ends_with(".so") {
                continue;
            }
            if !name.starts_with("libstd-") && !name.starts_with("libkithara_test_dylib") {
                continue;
            }
            let staged = libraries.join(name.as_ref());
            fs::copy(entry.path(), &staged)?;
            if is_shared_graph_library(&name) {
                strip_unneeded(toolchain, &staged, cancel)?;
            }
        }
    }
    Ok(())
}

fn strip_unneeded(toolchain: &Path, library: &Path, cancel: &child::Cancel) -> Result<()> {
    let status = child::run(
        Command::new(toolchain.join("bin/llvm-strip"))
            .arg("--strip-unneeded")
            .arg(library),
        Some(cancel),
    )?;
    if !status.success() {
        bail!("stripping {} failed", library.display());
    }
    Ok(())
}

fn retain_packaged_binaries(list: &Path, packaged: &BTreeMap<PathBuf, String>) -> Result<()> {
    let mut inventory: serde_json::Value = serde_json::from_slice(&fs::read(list)?)?;
    retain_packaged_inventory(&mut inventory, packaged)?;
    fs::write(list, serde_json::to_vec(&inventory)?)?;
    Ok(())
}

pub(super) fn retain_packaged_inventory(
    inventory: &mut serde_json::Value,
    packaged: &BTreeMap<PathBuf, String>,
) -> Result<()> {
    let Some(binaries) = inventory
        .get_mut("rust-binaries")
        .and_then(serde_json::Value::as_object_mut)
    else {
        bail!("binaries.json missing rust-binaries");
    };
    binaries.retain(|_, value| {
        value
            .get("binary-path")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|path| packaged.contains_key(Path::new(path)))
    });
    Ok(())
}

fn device_test_images(inventory: BinaryList) -> Result<Vec<PathBuf>> {
    let mut images = Vec::new();
    for (id, binary) in inventory.rust_binaries {
        match binary.build_platform.as_str() {
            "host" => continue,
            "target" => {}
            platform => bail!("unknown nextest build platform {platform}"),
        }
        // The nextest default filter drops this binary id at test selection,
        // and staging applies the same rule to its image.
        if id == "kithara-integration-tests" {
            continue;
        }
        images.push(binary.binary_path);
    }
    Ok(images)
}

fn package_libraries(
    list: &Path,
    libraries: &Path,
    toolchain: &Path,
    cancel: &child::Cancel,
) -> Result<BTreeMap<PathBuf, String>> {
    let inventory: BinaryList = serde_json::from_slice(&fs::read(list)?)?;
    let mut binaries = BTreeMap::new();
    let mut sizes = Vec::new();
    for image in device_test_images(inventory)? {
        let hash = hex::encode(Sha256::digest(image.as_os_str().as_encoded_bytes()));
        let name = format!("kithara_test_{hash}");
        let staged = libraries.join(format!("lib{name}.so"));
        fs::copy(&image, &staged)?;
        strip_unneeded(toolchain, &staged, cancel)?;
        sizes.push(serde_json::json!({"binary": image, "original_bytes": fs::metadata(&image)?.len(), "staged_bytes": fs::metadata(&staged)?.len(), "staged_sha256": sha256(&staged)?, "original_sha256": sha256(&image)?}));
        binaries.insert(image, name);
    }
    if binaries.is_empty() {
        bail!("nextest produced no Android test binaries");
    }
    fs::write(
        list.with_file_name("packaged-binaries.json"),
        serde_json::to_vec_pretty(&sizes)?,
    )?;
    Ok(binaries)
}

struct Configured {
    lease: FileLock,
    ndk: PathBuf,
    toolchain: PathBuf,
    cargo_target: PathBuf,
    environment: BTreeMap<String, String>,
}

fn configure(
    root: &Path,
    evidence: &Path,
    abi: &str,
    target: &str,
    cancel: &child::Cancel,
) -> Result<Configured> {
    let extension = KitharaExt::load(root)?;
    let api = &extension.android.api_level;
    let ndk = ndk_root()?;
    let toolchain = ndk_prebuilt()?;
    let source = root.join("android/native-test");
    let mut digest = Sha256::new();
    for file in ["native/bootstrap.rs", "native/bridge.c", "native/linker.sh"] {
        digest.update(fs::read(source.join(file))?);
    }
    digest.update(fs::read(root.join("xtask/src/android/native/link.rs"))?);
    digest.update(ndk.as_os_str().as_encoded_bytes());
    digest.update(target.as_bytes());
    digest.update(api.as_bytes());
    let target_dir = root.join(
        env::var_os("CARGO_TARGET_DIR").map_or_else(|| PathBuf::from("target"), PathBuf::from),
    );
    let cache = target_dir
        .join("android-native")
        .join(hex::encode(digest.finalize()));
    fs::create_dir_all(cache.join("objects"))?;
    let lock = fs::File::options()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(cache.join("run.lock"))?;
    let lease = FileLock::try_exclusive(lock)
        .context("another Android baseline owns this native build cache")?;
    let cargo_target = cache.join("cargo");
    let mut metadata_command = Command::new("cargo");
    metadata_command
        .current_dir(root)
        .args(["metadata", "--format-version", "1", "--locked"]);
    logged(
        &mut metadata_command,
        &evidence.join("cargo-metadata.json"),
        &evidence.join("metadata.log"),
        cancel,
    )?;
    let compiler = toolchain.join(format!("bin/{target}{api}-clang"));
    let bridge = cache.join(format!("bridge-{target}.o"));
    let mut cc = Command::new(&compiler);
    cc.args(["-fPIC", "-c"])
        .arg(source.join("native/bridge.c"))
        .arg("-o")
        .arg(&bridge);
    logged(
        &mut cc,
        &evidence.join("bridge.stdout.log"),
        &evidence.join("bridge.log"),
        cancel,
    )?;
    let link = link::Config {
        compiler,
        target: target.into(),
        source: source.join("native/bootstrap.rs"),
        bridge,
        objects: cache.join("objects"),
    };
    let link_path = cache.join(format!("link-{target}.json"));
    fs::write(&link_path, serde_json::to_vec_pretty(&link)?)?;
    let output = child::output(
        Command::new("cargo")
            .current_dir(root)
            .env("ANDROID_NDK_HOME", &ndk)
            .args(["ndk-env", "--target", abi, "--platform", api, "--json"]),
        Some(cancel),
        Duration::from_secs(30),
    )?;
    if !output.status.success() {
        bail!(
            "cargo ndk-env failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let mut environment: BTreeMap<String, String> = serde_json::from_slice(&output.stdout)?;
    // Native build scripts read the NDK out of the environment.
    environment.insert("ANDROID_NDK_HOME".into(), ndk.display().to_string());
    let target_key = target.replace('-', "_").to_ascii_uppercase();
    environment.remove(&format!("CARGO_TARGET_{target_key}_RUNNER"));
    environment.insert(
        format!("CARGO_TARGET_{target_key}_LINKER"),
        source.join("native/linker.sh").display().to_string(),
    );
    environment.insert(
        "KITHARA_ANDROID_LINK_CONFIG".into(),
        link_path.display().to_string(),
    );
    environment.insert(
        "KITHARA_ANDROID_XTASK".into(),
        env::current_exe()?.display().to_string(),
    );
    Ok(Configured {
        lease,
        ndk,
        toolchain,
        cargo_target,
        environment,
    })
}

fn install(
    root: &Path,
    evidence: &Path,
    ndk: &Path,
    device: &Selected,
    cancel: &child::Cancel,
) -> Result<String> {
    let gradle_build = evidence.join("apk");
    let mut gradle = Command::new(root.join("android/gradlew"));
    gradle
        .current_dir(root.join("android/native-test"))
        .env("ANDROID_HOME", android_sdk_root()?)
        .arg("assembleDebug")
        .arg("--no-daemon")
        .arg(format!("-Pkithara.nativeTestNdk={}", ndk.display()))
        .arg(format!(
            "-Pkithara.nativeTestLibraries={}",
            evidence.join("jniLibs").display()
        ))
        .arg(format!(
            "-Pkithara.nativeTestBuild={}",
            gradle_build.display()
        ));
    logged(
        &mut gradle,
        &evidence.join("apk.stdout.log"),
        &evidence.join("apk.log"),
        cancel,
    )?;
    let apk = gradle_build.join("outputs/apk/debug/kithara-native-test-debug.apk");
    let installed = child::output(
        device.adb().arg("install").arg("-r").arg(&apk),
        Some(cancel),
        Duration::from_secs(120),
    )?;
    fs::write(
        evidence.join("install.log"),
        [&installed.stdout[..], &installed.stderr[..]].concat(),
    )?;
    if !installed.status.success() {
        bail!("native test APK install failed; see install.log");
    }
    let home = child::output(
        device.adb().args(["shell", "run-as", PACKAGE, "pwd"]),
        Some(cancel),
        Duration::from_secs(10),
    )?;
    if !home.status.success() {
        bail!("resolving native test application directory failed");
    }
    Ok(String::from_utf8(home.stdout)?.trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inventory(entries: &str) -> BinaryList {
        serde_json::from_str(&format!(r#"{{"rust-binaries": {{{entries}}}}}"#))
            .expect("nextest inventory is valid JSON")
    }

    const ASSETS_LIB: &str = r#""kithara-assets": {
        "binary-id": "kithara-assets",
        "binary-name": "kithara_assets",
        "kind": "lib",
        "binary-path": "/deps/kithara_assets-1",
        "build-platform": "target"
    }"#;
    const ASSETS_TEST: &str = r#""kithara-assets::crash_recovery": {
        "binary-id": "kithara-assets::crash_recovery",
        "binary-name": "crash_recovery",
        "kind": "test",
        "binary-path": "/deps/crash_recovery-2",
        "build-platform": "target"
    }"#;
    const MACROS_PROC: &str = r#""kithara-test-macros": {
        "binary-id": "kithara-test-macros",
        "binary-name": "kithara_test_macros",
        "kind": "proc-macro",
        "binary-path": "/deps/kithara_test_macros-3",
        "build-platform": "host"
    }"#;
    const DEVTOOLS_LIB: &str = r#""kithara-devtools": {
        "binary-id": "kithara-devtools",
        "binary-name": "kithara_devtools",
        "kind": "lib",
        "binary-path": "/deps/kithara_devtools-4",
        "build-platform": "host"
    }"#;
    const INTEGRATION_LIB: &str = r#""kithara-integration-tests": {
        "binary-id": "kithara-integration-tests",
        "binary-name": "kithara_integration_tests",
        "kind": "lib",
        "binary-path": "/deps/kithara_integration_tests-5",
        "build-platform": "target"
    }"#;

    #[test]
    fn staging_takes_every_target_platform_test_image() {
        let images = device_test_images(inventory(&format!(
            "{ASSETS_LIB}, {ASSETS_TEST}, {MACROS_PROC}, {DEVTOOLS_LIB}"
        )))
        .expect("the inventory names known build platforms");
        assert!(images.contains(&PathBuf::from("/deps/kithara_assets-1")));
        assert!(images.contains(&PathBuf::from("/deps/crash_recovery-2")));
        assert_eq!(images.len(), 2);
    }

    #[test]
    fn staging_leaves_out_the_integration_helper_library() {
        let images = device_test_images(inventory(&format!("{INTEGRATION_LIB}, {ASSETS_TEST}")))
            .expect("the inventory names known build platforms");
        assert_eq!(images, vec![PathBuf::from("/deps/crash_recovery-2")]);
    }

    fn staged_entries(count: u64) -> Vec<(String, u64)> {
        (0..count)
            .map(|index| (format!("libkithara_test_{index}.so"), 64 * 1024 * 1024))
            .collect()
    }

    #[test]
    fn staging_accepts_a_total_above_a_gibibyte() {
        let mut entries = staged_entries(16);
        entries.push(("libc++_shared.so".into(), 1));
        refuse_oversized_staging(&entries).expect("a staging directory this size installs");
    }

    #[test]
    fn staging_refuses_a_total_above_the_install_budget() {
        let mut entries = staged_entries(32);
        entries.push(("libc++_shared.so".into(), 1));
        let error = refuse_oversized_staging(&entries)
            .expect_err("a staging directory this size blocks packaging")
            .to_string();
        assert!(error.contains("2147483649"), "{error}");
        assert!(error.contains("2147483648"), "{error}");
    }

    #[test]
    fn staging_refuses_an_image_that_missed_the_shared_graph() {
        let error = refuse_oversized_staging(&[
            ("libkithara_test_abc.so".into(), 64 * 1024 * 1024 + 1),
            ("libkithara_test_dylib.so".into(), 109 * 1024 * 1024),
        ])
        .expect_err("an image carrying the product graph blocks packaging")
        .to_string();
        assert!(error.contains("libkithara_test_abc.so=67108865"), "{error}");
        assert!(!error.contains("libkithara_test_dylib"), "{error}");
    }
}
