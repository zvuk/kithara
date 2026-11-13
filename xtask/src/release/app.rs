use std::{collections::BTreeMap, env, fs, path::Path};

use anyhow::{Context, Result, bail};
use cargo_metadata::MetadataCommand;
use kithara_devtools::Ctx;
use plist::{Dictionary, Value};

use super::{artifact::sha256, git::git};
use crate::{apple::copy_dir_all, ci::process::Process, consts};

pub(super) fn build(ctx: &Ctx, identity: Option<&str>) -> Result<()> {
    let process = Process::new(&ctx.root, BTreeMap::new());
    process.require_os(&["macos"], "macOS application packaging")?;
    let identity = identity
        .map(str::to_owned)
        .or_else(|| env::var("KITHARA_APP_SIGN_IDENTITY").ok())
        .filter(|value| !value.trim().is_empty() && value != "-")
        .context("set --identity or KITHARA_APP_SIGN_IDENTITY to a Keychain signing certificate")?;
    let identities = process.capture(
        "security",
        &["find-identity", "-v", "-p", "codesigning"],
        "available code-signing identities",
    )?;
    require_identity(&identities, &identity)?;

    let metadata = MetadataCommand::new()
        .current_dir(&ctx.root)
        .no_deps()
        .exec()
        .context("read application build metadata")?;
    let package = metadata
        .packages
        .iter()
        .find(|package| package.name == "kithara-app")
        .context("workspace has no kithara-app package")?;
    let version = &package.version;
    let short_version = format!("{}.{}.{}", version.major, version.minor, version.patch);
    let mut revision = git(&ctx.root, &["rev-parse", "HEAD"])?;
    if !git(&ctx.root, &["status", "--porcelain"])?.is_empty() {
        revision.push_str("-dirty");
    }
    let output = metadata.target_directory.as_std_path().join("release-app");
    fs::create_dir_all(&output)?;
    let staging = tempfile::Builder::new()
        .prefix(".packaging-")
        .tempdir_in(&output)?;
    let bundle = staging.path().join("Kithara.app");
    let contents = bundle.join("Contents");
    let executable = contents.join("MacOS/kithara");
    fs::create_dir_all(contents.join("MacOS"))?;
    let resources = contents.join("Resources");
    prepare_resources(&ctx.root, &resources)?;
    info(&short_version, &version.to_string(), &revision)
        .to_file_xml(contents.join("Info.plist"))?;
    icon(&process, &ctx.root, &resources, staging.path())?;

    build_slices(&process)?;
    let mut lipo = process.command("lipo");
    lipo.arg("-create");
    for target in consts::MACOS_APP_TARGETS {
        lipo.arg(
            metadata
                .target_directory
                .join(target)
                .join("release/kithara"),
        );
    }
    lipo.arg("-output").arg(&executable);
    process.run_command(&mut lipo, "assemble universal application")?;
    let mut strip = process.command("strip");
    strip.arg("-x").arg(&executable);
    process.run_command(&mut strip, "strip application symbols")?;
    let mut inspect = process.command("otool");
    inspect.args(["-arch", "all", "-L"]).arg(&executable);
    let linkage = process.capture_command(&mut inspect, "application linkage")?;
    require_system_libraries(&linkage)?;
    sign(&process, &bundle, &identity)?;

    let archive = staging.path().join(consts::MACOS_APP_ARCHIVE);
    let mut zip = process.command("ditto");
    zip.args(["-c", "-k", "--sequesterRsrc", "--keepParent"])
        .arg(&bundle)
        .arg(&archive);
    process.run_command(&mut zip, "archive signed application")?;
    fs::write(
        staging
            .path()
            .join(format!("{}.sha256", consts::MACOS_APP_ARCHIVE)),
        format!("{}  {}\n", sha256(&archive)?, consts::MACOS_APP_ARCHIVE),
    )?;
    let installed = output.join("Kithara.app");
    if installed.exists() {
        fs::remove_dir_all(&installed)?;
    }
    fs::rename(&bundle, &installed)?;
    for name in [
        consts::MACOS_APP_ARCHIVE.to_owned(),
        format!("{}.sha256", consts::MACOS_APP_ARCHIVE),
    ] {
        let pending = tempfile::NamedTempFile::new_in(&ctx.root)?;
        fs::copy(staging.path().join(&name), pending.path())?;
        pending.persist(ctx.root.join(&name))?;
    }
    tracing::info!(app = %installed.display(), archive = consts::MACOS_APP_ARCHIVE, %revision, %identity, "macOS application packaged; notarization was not performed");
    Ok(())
}

fn build_slices(process: &Process) -> Result<()> {
    for target in consts::MACOS_APP_TARGETS {
        let mut command = process.command("cargo");
        command
            .args([
                "build",
                "--locked",
                "--release",
                "-p",
                "kithara-app",
                "--bin",
                "kithara",
                "--features",
                "zvuk,keystore",
                "--target",
                target,
            ])
            .env("MACOSX_DEPLOYMENT_TARGET", consts::MACOS_APP_MINIMUM_OS)
            .env("KITHARA_DRM_REQUIRE", "1");
        process.run_command(&mut command, "build application release slice")?;
    }
    Ok(())
}

fn require_identity(available: &str, identity: &str) -> Result<()> {
    let quoted = format!("\"{identity}\"");
    let found = available.lines().any(|line| {
        let mut words = line.split_whitespace();
        words.next().is_some_and(|word| word.ends_with(')'))
            && (words.next() == Some(identity) || line.trim_end().ends_with(&quoted))
    });
    if !found {
        bail!("the requested signing identity is not available in Keychain: {identity}");
    }
    Ok(())
}

fn prepare_resources(root: &Path, resources: &Path) -> Result<()> {
    let ui = resources.join("assets/ui");
    copy_dir_all(&root.join("crates/kithara-ui/assets"), &ui)?;
    copy_dir_all(&root.join("crates/kithara-app/assets/ui"), &ui)?;
    for name in ["LICENSE-MIT", "LICENSE-APACHE"] {
        fs::copy(root.join(name), resources.join(name))?;
    }
    fs::copy(
        root.join("crates/kithara-app/app.yaml"),
        resources.join("app.yaml"),
    )?;
    fs::write(resources.join("README.txt"), include_str!("app-readme.txt"))?;
    Ok(())
}

fn info(short_version: &str, version: &str, revision: &str) -> Value {
    let mut fields = Dictionary::new();
    for (key, value) in [
        ("CFBundleExecutable", "kithara"),
        ("CFBundleIdentifier", "com.kithara.app"),
        ("CFBundleName", "Kithara"),
        ("CFBundleDisplayName", "Kithara"),
        ("CFBundleInfoDictionaryVersion", "6.0"),
        ("CFBundlePackageType", "APPL"),
        ("CFBundleShortVersionString", short_version),
        ("CFBundleVersion", short_version),
        ("CFBundleIconFile", "Kithara.icns"),
        ("LSMinimumSystemVersion", consts::MACOS_APP_MINIMUM_OS),
        ("LSApplicationCategoryType", "public.app-category.music"),
        ("KitharaVersion", version),
        ("KitharaRevision", revision),
    ] {
        fields.insert(key.into(), Value::String(value.into()));
    }
    fields.insert("NSHighResolutionCapable".into(), Value::Boolean(true));
    Value::Dictionary(fields)
}

fn icon(process: &Process, root: &Path, resources: &Path, staging: &Path) -> Result<()> {
    let iconset = staging.join("Kithara.iconset");
    fs::create_dir_all(&iconset)?;
    let mut raster = process.command("sips");
    raster
        .args(["-s", "format", "png", "-z", "1024", "1024"])
        .arg(root.join("crates/kithara-ui/assets/icons/kithara.svg"))
        .arg("--out")
        .arg(iconset.join("icon_512x512@2x.png"));
    process.run_command(&mut raster, "render application icon")?;
    let mut icon = process.command("iconutil");
    icon.args(["-c", "icns"])
        .arg(&iconset)
        .arg("-o")
        .arg(resources.join("Kithara.icns"));
    process.run_command(&mut icon, "assemble application icon")
}

fn require_system_libraries(linkage: &str) -> Result<()> {
    let mut libraries = 0;
    for line in linkage.lines().filter(|line| line.starts_with('\t')) {
        let library = line
            .split_whitespace()
            .next()
            .context("empty dylib entry")?;
        if !library.starts_with("/System/Library/") && !library.starts_with("/usr/lib/") {
            bail!("application depends on a non-system library: {library}");
        }
        libraries += 1;
    }
    if libraries == 0 {
        bail!("application linkage inspection reported no libraries");
    }
    Ok(())
}

fn sign(process: &Process, bundle: &Path, identity: &str) -> Result<()> {
    let mut command = process.command("codesign");
    command
        .args([
            "--force",
            "--options",
            "runtime",
            "--timestamp",
            "--sign",
            identity,
        ])
        .arg(bundle);
    process.run_command(&mut command, "sign macOS application")?;
    let mut verify = process.command("codesign");
    verify.args(["--verify", "--deep", "--strict"]).arg(bundle);
    process.run_command(&mut verify, "verify macOS application signature")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ci::process::Recording, config::KitharaExt};

    #[test]
    fn release_slices_include_zvuk_and_native_credentials() {
        let process = Process::recording(Path::new("/checkout"), Recording::default());
        build_slices(&process).unwrap();
        let recording = process.recorded().unwrap();
        assert_eq!(recording.steps().len(), consts::MACOS_APP_TARGETS.len());
        for step in recording.steps() {
            assert!(
                step.args
                    .windows(2)
                    .any(|args| args == ["--features", "zvuk,keystore"])
            );
            assert_eq!(
                step.env.get("KITHARA_DRM_REQUIRE").map(String::as_str),
                Some("1")
            );
        }
    }

    #[test]
    fn packaging_carries_the_real_ui_and_stock_zvuk_configuration() {
        let root = crate::testing::workspace::root();
        let temp = tempfile::tempdir().unwrap();
        prepare_resources(&root, temp.path()).unwrap();
        for name in [
            "package.kpackage.ron",
            "app.klayout.ron",
            "app-single.klayout.ron",
            "kithara-dark.kskin.ron",
            "kithara-en.ktext.ron",
            "app-en.ktext.ron",
            "modules/app-deck.kmodule.ron",
            "sprites/spinner.png",
            "icons/kithara.svg",
        ] {
            assert!(temp.path().join("assets/ui").join(name).is_file(), "{name}");
        }
        let text = fs::read_to_string(temp.path().join("app.yaml")).unwrap();
        assert_eq!(
            text,
            fs::read_to_string(root.join("crates/kithara-app/app.yaml")).unwrap()
        );
        let config: serde_yaml_ng::Value = serde_yaml_ng::from_str(&text).unwrap();
        assert!(
            config["sources"]["zvuk"]["user_agent"]
                .as_str()
                .is_some_and(|value| !value.is_empty())
        );
        assert!(
            config["drm"]["providers"]
                .as_sequence()
                .unwrap()
                .iter()
                .all(|provider| provider["cipher_key"]
                    .as_str()
                    .is_some_and(|value| value.starts_with("$KITHARA_")))
        );
        assert!(!temp.path().join(".env").exists());
        let project = Ctx::load().unwrap();
        let ext = KitharaExt::from_ctx(&project).unwrap();
        assert!(
            ext.release
                .assets()
                .any(|name| name == consts::MACOS_APP_ARCHIVE)
        );
    }

    #[test]
    fn non_system_linkage_and_missing_identity_fail_closed() {
        require_system_libraries(
            "app:\n\t/usr/lib/libSystem.B.dylib (compatibility version 1.0)\n",
        )
        .unwrap();
        for library in [
            "/opt/homebrew/lib/libfoo.dylib",
            "@rpath/libfoo.dylib",
            "relative.dylib",
        ] {
            assert!(require_system_libraries(&format!("app:\n\t{library} (version 1)\n")).is_err());
        }
        assert!(require_system_libraries("app:\n").is_err());
        let available =
            "  1) ABC123 \"Apple Development: Test (TEAM)\"\n     1 valid identities found\n";
        require_identity(available, "ABC123").unwrap();
        require_identity(available, "Apple Development: Test (TEAM)").unwrap();
        assert!(require_identity(available, "Apple Development").is_err());
        assert!(require_identity(available, "-").is_err());
    }

    #[test]
    fn bundle_metadata_and_signing_describe_the_delivered_code() {
        let plist = info("1.2.3", "1.2.3-beta1", "abc123");
        assert_eq!(
            plist.as_dictionary().unwrap()["CFBundlePackageType"].as_string(),
            Some("APPL")
        );
        assert_eq!(
            plist.as_dictionary().unwrap()["KitharaRevision"].as_string(),
            Some("abc123")
        );
        let process = Process::recording(Path::new("/checkout"), Recording::default());
        sign(&process, Path::new("/Some Folder/Kithara.app"), "ABC123").unwrap();
        let recording = process.recorded().unwrap();
        let [sign, verify] = recording.steps() else {
            panic!("sign and verify");
        };
        assert_eq!(sign.program, "codesign");
        assert!(
            sign.args
                .windows(2)
                .any(|args| args == ["--sign", "ABC123"])
        );
        assert_eq!(
            verify.args,
            ["--verify", "--deep", "--strict", "/Some Folder/Kithara.app"]
        );
    }
}
