use std::{
    env, fs,
    path::{Path as FsPath, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{Context, Result, bail};
use cargo_metadata::{Message, MetadataCommand};
use kithara_devtools::{Ctx, common::tools::ToolsConfig};
use plist::{Dictionary as PlistDictionary, Value as PlistValue};
use regex::Regex;

use crate::{
    apple_docgen,
    config::{AppleConfig, KitharaExt, ReleaseConfig},
    consts,
};

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
pub(crate) fn copy_dir_all(src: &FsPath, dst: &FsPath) -> Result<()> {
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

struct HakariDisableGuard {
    manifest: PathBuf,
    original_manifest: String,
    lockfile: PathBuf,
    original_lockfile: String,
    active: bool,
}

impl HakariDisableGuard {
    fn disable(workspace_root: &FsPath) -> Result<Self> {
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
fn audit_symbols(xcframework_dir: &FsPath, apple: &AppleConfig, tools: &ToolsConfig) -> Result<()> {
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
    for slice in consts::XCFRAMEWORK_SLICES {
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
        consts::XCFRAMEWORK_SLICES.len()
    );
    Ok(())
}

/// The sysroot's `llvm-nm` when the toolchain ships one, and the configured
/// `nm` otherwise. `archive_strings` resolves `strings` through the table for
/// the same audit, so the fallback resolves too — half a configurable
/// operation sends a machine that redirected one tool to the wrong other one.
fn symbol_tool(tools: &ToolsConfig) -> PathBuf {
    crate::sysroot::tool("llvm-nm").unwrap_or_else(|| PathBuf::from(tools.program("nm")))
}

fn archive_strings(lib: &FsPath, tools: &ToolsConfig) -> Result<String> {
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

fn slice_build_plan<'a>(
    slices: &[(&str, &[&'a str])],
    target: Option<&'a str>,
) -> Vec<Vec<&'a str>> {
    target.map_or_else(
        || slices.iter().map(|(_, targets)| targets.to_vec()).collect(),
        |target| vec![vec![target]],
    )
}

fn run_build(
    profile: crate::BuildProfile,
    target: Option<&str>,
    tools: &ToolsConfig,
) -> Result<()> {
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

    println!("==> Building Apple static libraries");
    let features = device_features("apple", true);
    let plan = slice_build_plan(consts::SLICE_TARGETS, target);
    let mut builds = Vec::new();
    for triple in plan.iter().flatten() {
        let (archive, libs) = build_slice_staticlib(
            &crate_dir,
            profile,
            triple,
            &deployment_target,
            &features,
            tools,
        )?;
        builds.push((*triple, archive, libs));
    }
    let (_, archive, _) = builds.first().context("no Apple targets to build")?;
    let generated_dir = crate_dir.join("generated");
    let (swift_src, headers) = generate_apple_bindings(archive, &generated_dir)?;
    let ffi_module = headers
        .file_name()
        .context("generated headers have no module directory")?;
    let xcf_dst = apple_dir.join(ffi_module).with_extension("xcframework");
    let swift_name = swift_src
        .file_name()
        .context("generated Swift has no filename")?;
    let swift_dst = apple_dir
        .join("Sources")
        .join(
            swift_src
                .file_stem()
                .context("generated Swift has no module name")?,
        )
        .join(swift_name);
    println!("==> Copying outputs to apple/");
    assemble_staticlib_xcframework(&xcf_dst, &plan, &builds, &headers, tools)?;
    apply_slice_link_directives(&xcf_dst, &builds, ffi_module, target)?;
    if matches!(profile, crate::BuildProfile::Release) {
        strip_xcframework(&xcf_dst, tools)?;
    }
    link_like_a_consumer(&xcf_dst, tools)?;

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

fn generate_apple_bindings(archive: &FsPath, out_dir: &FsPath) -> Result<(PathBuf, PathBuf)> {
    if out_dir.exists() {
        fs::remove_dir_all(out_dir).with_context(|| format!("remove {}", out_dir.display()))?;
    }
    uniffi_bindgen::bindings::generate(uniffi_bindgen::bindings::GenerateOptions {
        languages: vec![uniffi_bindgen::bindings::TargetLanguage::Swift],
        source: archive
            .to_path_buf()
            .try_into()
            .context("archive path is not UTF-8")?,
        out_dir: out_dir
            .to_path_buf()
            .try_into()
            .context("bindings path is not UTF-8")?,
        metadata_no_deps: true,
        ..Default::default()
    })
    .with_context(|| format!("generate Swift bindings from {}", archive.display()))?;
    let sources = swift_files(out_dir)?;
    let [swift] = sources.as_slice() else {
        bail!(
            "expected one generated Swift binding, got {}",
            sources.len()
        );
    };
    let headers = fs::read_dir(out_dir)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<Vec<_>>>()?
        .into_iter()
        .filter(|path| path.extension().is_some_and(|extension| extension == "h"))
        .collect::<Vec<_>>();
    let [header] = headers.as_slice() else {
        bail!("expected one generated C header, got {}", headers.len());
    };
    let header_dir = out_dir.join("Headers").join(
        header
            .file_stem()
            .context("generated header has no module name")?,
    );
    fs::create_dir_all(&header_dir)?;
    fs::copy(
        header,
        header_dir.join(header.file_name().context("header has no filename")?),
    )?;
    let modulemap = header.with_extension("modulemap");
    fs::copy(&modulemap, header_dir.join("module.modulemap"))
        .with_context(|| format!("copy {}", modulemap.display()))?;
    Ok((swift.clone(), header_dir))
}

fn assemble_staticlib_xcframework(
    xcframework: &FsPath,
    plan: &[Vec<&str>],
    builds: &[(&str, PathBuf, Vec<NativeLib>)],
    headers: &FsPath,
    tools: &ToolsConfig,
) -> Result<()> {
    let staging = tempfile::Builder::new()
        .prefix(&format!(
            "{}-apple-staticlibs",
            kithara_devtools::util::project_name()
        ))
        .tempdir()?;
    let mut command = Command::new(tools.program("xcodebuild"));
    command.arg("-create-xcframework");
    for (index, targets) in plan.iter().enumerate() {
        let archives = targets
            .iter()
            .map(|target| {
                builds
                    .iter()
                    .find_map(|(triple, archive, _)| {
                        (*triple == *target).then_some(archive.clone())
                    })
                    .with_context(|| format!("missing staticlib build for {target}"))
            })
            .collect::<Result<Vec<_>>>()?;
        let archive = match archives.as_slice() {
            [archive] => archive.clone(),
            [] => bail!("xcframework slice has no targets"),
            _ => {
                let directory = staging.path().join(index.to_string());
                fs::create_dir_all(&directory)?;
                let name = archives[0]
                    .file_name()
                    .context("staticlib has no filename")?;
                let archive = directory.join(name);
                lipo_create(&archives, &archive, tools)?;
                archive
            }
        };
        command.arg("-library").arg(archive).arg("-headers").arg(
            headers
                .parent()
                .context("generated headers have no parent")?,
        );
    }
    if xcframework.exists() {
        fs::remove_dir_all(xcframework)
            .with_context(|| format!("remove {}", xcframework.display()))?;
    }
    command.arg("-output").arg(xcframework);
    run_quiet(&mut command, "create staticlib xcframework")
}

fn device_features(platform: &str, dev: bool) -> String {
    let mut features = format!("uniffi,{platform}");
    if dev {
        features.push_str(",dev");
    }
    let selected = env::var("KITHARA_FFI_FEATURES").unwrap_or_else(|_| "standard".to_owned());
    if !selected.trim().is_empty() {
        features.push(',');
        features.push_str(selected.trim());
    }
    features
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
/// cargo's `TARGET`. For the simulator slice that yields `arm64-apple-ios-sim`,
/// but libclang wants `-simulator`; pin a valid simulator triple and sysroot.
fn set_simulator_bindgen_args(cmd: &mut Command, tools: &ToolsConfig) -> Result<()> {
    let sim_sdk = sdk_path("iphonesimulator", tools)?;
    let sim_sdk = sim_sdk
        .to_str()
        .context("iphonesimulator SDK path is not UTF-8")?;
    cmd.env(
        "BINDGEN_EXTRA_CLANG_ARGS_aarch64_apple_ios_sim",
        format!("--target=arm64-apple-ios-simulator -isysroot {sim_sdk}"),
    );
    Ok(())
}

fn set_release_rustflags(cmd: &mut Command) {
    let mut flags = env::var("CARGO_ENCODED_RUSTFLAGS").unwrap_or_default();
    for flag in consts::RELEASE_RUSTFLAGS {
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

#[derive(Debug, Clone, PartialEq, Eq)]
enum NativeLib {
    Framework(String),
    Library(String),
}

fn parse_native_static_libs(note: &str) -> Result<Vec<NativeLib>> {
    let tokens = note
        .strip_prefix("native-static-libs:")
        .with_context(|| format!("missing native-static-libs: prefix in `{note}`"))?;
    let mut tokens = tokens.split_whitespace();
    let mut libs = Vec::new();
    while let Some(token) = tokens.next() {
        let lib = if token == "-framework" {
            let name = tokens.next().context("missing name after `-framework`")?;
            if name.starts_with('-') {
                bail!("invalid framework name token `{name}` after `-framework`");
            }
            NativeLib::Framework(name.to_owned())
        } else if let Some(name) = token.strip_prefix("-l").filter(|name| !name.is_empty()) {
            NativeLib::Library(name.to_owned())
        } else {
            bail!("unknown native-static-libs token `{token}`");
        };
        if !libs.contains(&lib) {
            libs.push(lib);
        }
    }
    Ok(libs)
}

fn with_link_directives(modulemap: &str, libs: &[NativeLib]) -> Result<String> {
    let closing = modulemap
        .rfind('}')
        .context("modulemap has no closing `}`")?;
    let mut output = modulemap[..closing].trim_end().to_owned();
    output.push('\n');
    for lib in libs {
        let line = match lib {
            NativeLib::Framework(name) => format!("    link framework \"{name}\"\n"),
            NativeLib::Library(name) => format!("    link \"{name}\"\n"),
        };
        output.push_str(&line);
    }
    output.push_str(&modulemap[closing..]);
    Ok(output)
}

fn union_native_libs(lists: &[Vec<NativeLib>]) -> Vec<NativeLib> {
    let mut union = Vec::new();
    for lib in lists.iter().flatten() {
        if !union.contains(lib) {
            union.push(lib.clone());
        }
    }
    union
}

/// Each slice ships the archive whose native link list rustc printed, so its
/// modulemap always describes what ships.
fn apply_slice_link_directives(
    xcframework: &FsPath,
    builds: &[(&str, PathBuf, Vec<NativeLib>)],
    ffi_module: &std::ffi::OsStr,
    target: Option<&str>,
) -> Result<()> {
    require_dir(xcframework)?;
    for entry in
        fs::read_dir(xcframework).with_context(|| format!("read {}", xcframework.display()))?
    {
        let slice_dir = entry?.path();
        if !slice_dir.is_dir() {
            continue;
        }
        let slice = slice_dir
            .file_name()
            .and_then(|name| name.to_str())
            .with_context(|| format!("slice name is not UTF-8: {}", slice_dir.display()))?;
        let targets = match target.as_ref() {
            Some(target) => std::slice::from_ref(target),
            None => consts::SLICE_TARGETS
                .iter()
                .find_map(|(name, targets)| (*name == slice).then_some(*targets))
                .with_context(|| {
                    format!("no target triples registered for xcframework slice `{slice}`")
                })?,
        };

        let lists = targets
            .iter()
            .map(|target| {
                builds
                    .iter()
                    .find_map(|(triple, _, libs)| (*triple == *target).then_some(libs.clone()))
                    .with_context(|| format!("missing native link list for {target}"))
            })
            .collect::<Result<Vec<_>>>()?;
        let modulemap = slice_dir
            .join("Headers")
            .join(ffi_module)
            .join("module.modulemap");
        let source = fs::read_to_string(&modulemap)
            .with_context(|| format!("read {}", modulemap.display()))?;
        fs::write(
            &modulemap,
            with_link_directives(&source, &union_native_libs(&lists))?,
        )
        .with_context(|| format!("write {}", modulemap.display()))?;
    }
    Ok(())
}

/// Build one target's `kithara-ffi` archive as a `staticlib`-only unit and
/// return its reported path and native libraries. Overriding the manifest
/// `crate-type` lets cargo turn fat LTO on for the release unit.
fn build_slice_staticlib(
    crate_dir: &FsPath,
    profile: crate::BuildProfile,
    target: &str,
    deployment_target: &str,
    features: &str,
    tools: &ToolsConfig,
) -> Result<(PathBuf, Vec<NativeLib>)> {
    let mut cmd = Command::new("cargo");
    if matches!(profile, crate::BuildProfile::Release) {
        cmd.args(consts::RELEASE_CARGO_ARGS);
    }
    cmd.args(["rustc", "-p", "kithara-ffi"]);
    if matches!(profile, crate::BuildProfile::Release) {
        cmd.arg("--release");
        set_release_rustflags(&mut cmd);
    }
    cmd.args([
        "--target",
        target,
        "--no-default-features",
        "-F",
        features,
        "--crate-type",
        "staticlib",
        "--message-format=json",
    ]);
    cmd.args(["--", "--print", "native-static-libs"]);
    cmd.current_dir(crate_dir);
    cmd.env("IPHONEOS_DEPLOYMENT_TARGET", deployment_target);
    set_simulator_bindgen_args(&mut cmd, tools)?;

    let output = cmd
        .stderr(Stdio::inherit())
        .output()
        .with_context(|| format!("failed to run cargo rustc for {target}"))?;
    let mut artifact = None;
    let mut notes = Vec::new();
    let mut build_failed = false;
    for message in Message::parse_stream(output.stdout.as_slice()) {
        match message.with_context(|| format!("read cargo rustc messages for {target}"))? {
            Message::CompilerArtifact(compiled)
                if compiled.manifest_path.as_std_path() == crate_dir.join("Cargo.toml") =>
            {
                if let Some(archive) = compiled
                    .filenames
                    .iter()
                    .find(|path| path.extension() == Some("a"))
                {
                    artifact = Some((compiled.package_id, archive.clone().into_std_path_buf()));
                }
            }
            Message::CompilerMessage(diagnostic) => {
                if let Some(rendered) = &diagnostic.message.rendered {
                    eprint!("{rendered}");
                }
                if diagnostic
                    .message
                    .message
                    .starts_with("native-static-libs:")
                {
                    notes.push((diagnostic.package_id, diagnostic.message.message));
                }
            }
            Message::BuildFinished(finished) => build_failed |= !finished.success,
            _ => {}
        }
    }
    if !output.status.success() || build_failed {
        bail!("staticlib build failed for {target}");
    }
    let (package, lib) =
        artifact.with_context(|| format!("missing kithara-ffi archive for {target}"))?;
    let notes = notes
        .into_iter()
        .filter_map(|(owner, note)| (owner == package).then_some(note))
        .collect::<Vec<_>>();
    let [note] = notes.as_slice() else {
        bail!(
            "expected exactly one native-static-libs note for kithara-ffi ({target}), got {}",
            notes.len()
        );
    };
    require_file(&lib)?;
    Ok((lib, parse_native_static_libs(note)?))
}

fn strip_xcframework(xcframework: &FsPath, tools: &ToolsConfig) -> Result<()> {
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

/// Prove every internal slice autolinks its Rust dependencies without consumer
/// flags. Force-load the archive at the SDK version so dead stripping and Swift
/// compatibility libraries cannot hide missing system libraries.
fn link_like_a_consumer(xcframework: &FsPath, tools: &ToolsConfig) -> Result<()> {
    let plist = xcframework.join("Info.plist");
    let root =
        PlistValue::from_file(&plist).with_context(|| format!("read {}", plist.display()))?;
    let libraries = root
        .as_dictionary()
        .and_then(|dict| dict.get("AvailableLibraries"))
        .and_then(PlistValue::as_array)
        .with_context(|| format!("invalid xcframework plist {}", plist.display()))?;
    let temp = tempfile::Builder::new()
        .prefix(&format!(
            "{}-apple-consumer",
            kithara_devtools::util::project_name()
        ))
        .tempdir()?;
    let source = temp.path().join("main.swift");
    fs::write(&source, "import KitharaFFIInternal\n")
        .with_context(|| format!("write {}", source.display()))?;

    for library in libraries {
        let dict = library
            .as_dictionary()
            .with_context(|| format!("invalid xcframework library entry: {library:?}"))?;
        let identifier = dict
            .get("LibraryIdentifier")
            .and_then(PlistValue::as_string)
            .with_context(|| format!("missing LibraryIdentifier in {library:?}"))?;
        let headers = dict
            .get("HeadersPath")
            .and_then(PlistValue::as_string)
            .with_context(|| format!("missing HeadersPath for {identifier}"))?;
        let library_path = dict
            .get("LibraryPath")
            .and_then(PlistValue::as_string)
            .with_context(|| format!("missing LibraryPath for {identifier}"))?;
        let architectures = dict
            .get("SupportedArchitectures")
            .and_then(PlistValue::as_array)
            .with_context(|| format!("missing SupportedArchitectures for {identifier}"))?;
        let (sdk, os, suffix) = consumer_platform(dict)?;
        let sdk_dir = sdk_path(sdk, tools)?;
        let program = tools.program("xcrun");
        let output = Command::new(program)
            .args(["--sdk", sdk, "--show-sdk-version"])
            .output()
            .with_context(|| format!("{program} --show-sdk-version {sdk} for {identifier}"))?;
        if !output.status.success() {
            bail!(
                "{program} --show-sdk-version {sdk} for {identifier} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let version = String::from_utf8_lossy(&output.stdout);
        let slice = xcframework.join(identifier);
        for architecture in architectures {
            let arch = architecture.as_string().with_context(|| {
                format!("invalid architecture for {identifier}: {architecture:?}")
            })?;
            let triple = format!("{arch}-apple-{os}{}{suffix}", version.trim());
            let mut cmd = Command::new(tools.program("xcrun"));
            cmd.args(["swiftc", "-sdk"])
                .arg(&sdk_dir)
                .arg("-target")
                .arg(&triple)
                .arg("-I")
                .arg(slice.join(headers))
                .arg(&source)
                .args(["-Xlinker", "-force_load", "-Xlinker"])
                .arg(slice.join(library_path))
                .arg("-o")
                .arg(temp.path().join(format!("{identifier}-{arch}")));
            run_quiet(
                &mut cmd,
                &format!("link consumer for {identifier} ({arch})"),
            )?;
        }
    }
    Ok(())
}

/// Map one xcframework library entry to its SDK, target OS, and target suffix.
fn consumer_platform(
    library: &PlistDictionary,
) -> Result<(&'static str, &'static str, &'static str)> {
    let platform = library
        .get("SupportedPlatform")
        .and_then(PlistValue::as_string);
    let variant = library.get("SupportedPlatformVariant");
    match (platform, variant) {
        (Some("ios"), None) => Ok(("iphoneos", "ios", "")),
        (Some("ios"), Some(PlistValue::String(variant))) if variant == "simulator" => {
            Ok(("iphonesimulator", "ios", "-simulator"))
        }
        (Some("macos"), None) => Ok(("macosx", "macos", "")),
        _ => bail!("unsupported xcframework library entry: {library:?}"),
    }
}

fn zip_dir(
    parent: &FsPath,
    directory_name: &str,
    output: &FsPath,
    tools: &ToolsConfig,
) -> Result<()> {
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

fn swift_checksum(zip: &FsPath, tools: &ToolsConfig) -> Result<String> {
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
    sdk: &'a FsPath,
    module_map: &'a FsPath,
    rust_lib: &'a FsPath,
    out: &'a FsPath,
    module_triple: &'a str,
    rx_out: &'a FsPath,
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
    if matches!(profile, crate::BuildProfile::Release) && !built_internal {
        strip_xcframework(&internal, tools)?;
    }

    let rx_src = resolve_rxswift(&root, tools)?;
    println!("==> RxSwift source: {}", rx_src.display());

    let temp = tempfile::Builder::new()
        .prefix(&format!(
            "{}-apple-single",
            kithara_devtools::util::project_name()
        ))
        .tempdir()?;
    let work = temp.path().to_path_buf();
    let merged = work.join("merged");
    fs::create_dir_all(&merged)?;

    println!("==> Merging the Swift layers into one module");
    merge_sources(&apple_dir, &merged)?;

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
fn resolve_rxswift(root: &FsPath, tools: &ToolsConfig) -> Result<PathBuf> {
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
fn merge_sources(apple_dir: &FsPath, merged: &FsPath) -> Result<()> {
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
            fs::write(merged.join(FsPath::new(name)), transform_layer(&content)?)?;
        }
    }

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
    merged: &FsPath,
    internal: &FsPath,
    rx_src: &FsPath,
    work: &FsPath,
    out: &FsPath,
    spec: &SingleFrameworkSpec,
    tools: &ToolsConfig,
) -> Result<()> {
    let ios_sdk = sdk_path("iphoneos", tools)?;
    let sim_sdk = sdk_path("iphonesimulator", tools)?;

    let mm_dev = internal.join("ios-arm64/Headers/KitharaFFIInternal");
    let mm_sim = internal.join(format!(
        "{}/Headers/KitharaFFIInternal",
        consts::IOS_SIMULATOR_SLICE
    ));
    let rust_dev = internal.join("ios-arm64/libkithara_ffi.a");
    let rust_sim = internal.join(format!("{}/libkithara_ffi.a", consts::IOS_SIMULATOR_SLICE));
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
    )?;
    let [device, simulator] = &slices;
    for (arch, framework_dir) in [(device, &fw_ios), (simulator, &fw_sim)] {
        let binary = framework_dir.join(&framework).join(&spec.framework_name);
        link_single_like_a_consumer(arch, &binary, tools)?;
    }
    Ok(())
}

/// Prove an assembled single-framework archive autolinks its Rust dependencies
/// when force-loaded by an empty consumer alongside the matching `RxSwift` archive.
fn link_single_like_a_consumer(
    arch: &ArchBuild,
    framework: &FsPath,
    tools: &ToolsConfig,
) -> Result<()> {
    let source = arch.out.join("main.swift");
    fs::write(&source, "").with_context(|| format!("write {}", source.display()))?;
    // At the framework's 15.x target, Swift compatibility adds -lc++;
    // only the internal xcframework probe can detect a lost libc++.
    let mut cmd = Command::new(tools.program("xcrun"));
    cmd.args(["swiftc", "-sdk"])
        .arg(arch.sdk)
        .arg("-target")
        .arg(arch.triple)
        .arg(&source)
        .args(["-Xlinker", "-force_load", "-Xlinker"])
        .arg(framework)
        .arg(arch.rx_out.join("libRxSwift.a"))
        .arg("-o")
        .arg(arch.out.join("consumer"));
    run_quiet(
        &mut cmd,
        &format!("link single-framework consumer for {}", arch.triple),
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
    sdk: &FsPath,
    rx_files: &[PathBuf],
    rx_out: &FsPath,
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
    swift_lib: &FsPath,
    rust_lib: &FsPath,
    out_lib: &FsPath,
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
    fw_dir: &FsPath,
    platform: &str,
    slices: &[&FsPath],
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
fn create_xcframework(frameworks: &[&FsPath], out: &FsPath, tools: &ToolsConfig) -> Result<()> {
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
fn verify_single(out: &FsPath) -> Result<()> {
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
fn lipo_create(thin: &[PathBuf], out: &FsPath, tools: &ToolsConfig) -> Result<()> {
    let program = tools.program("lipo");
    let mut cmd = Command::new(program);
    cmd.arg("-create").args(thin).arg("-output").arg(out);
    run_quiet(&mut cmd, &format!("{program} create"))
}

fn require_dir(path: &FsPath) -> Result<()> {
    if !path.is_dir() {
        bail!("required directory is missing: {}", path.display());
    }
    Ok(())
}

fn require_file(path: &FsPath) -> Result<()> {
    if !path.is_file() {
        bail!("required file is missing: {}", path.display());
    }
    Ok(())
}

/// Top-level `.swift` files in `dir`, sorted.
fn swift_files(dir: &FsPath) -> Result<Vec<PathBuf>> {
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
fn swift_files_recursive(dir: &FsPath) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    collect_swift(dir, &mut files)?;
    files.sort();
    Ok(files)
}

fn public_swiftinterfaces(dir: &FsPath) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    collect_public_swiftinterfaces(dir, &mut files)?;
    files.sort();
    Ok(files)
}

fn collect_swift(dir: &FsPath, files: &mut Vec<PathBuf>) -> Result<()> {
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

fn collect_public_swiftinterfaces(dir: &FsPath, files: &mut Vec<PathBuf>) -> Result<()> {
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
fn write_info_plist(path: &FsPath, platform: &str, spec: &SingleFrameworkSpec) -> Result<()> {
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

fn locate_built_app(demo_dir: &FsPath, scheme: &str, configuration: &str) -> Result<PathBuf> {
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

fn first_existing_app(products_dir: &FsPath, scheme: &str) -> Option<PathBuf> {
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
    fn build_plan_preserves_each_slice_and_its_architectures() {
        let slices: &[(&str, &[&str])] = &[
            ("device", &["device-target"]),
            ("universal", &["first-target", "second-target"]),
        ];
        assert_eq!(
            slice_build_plan(slices, None),
            vec![vec!["device-target"], vec!["first-target", "second-target"]]
        );
    }

    #[test]
    fn build_plan_explicit_target_replaces_all_registered_slices() {
        let slices: &[(&str, &[&str])] = &[("unused", &["unused-target"])];
        assert_eq!(
            slice_build_plan(slices, Some("selected-target")),
            vec![vec!["selected-target"]]
        );
    }

    #[test]
    fn native_libraries_keep_first_seen_order_without_duplicates() {
        assert_eq!(
            parse_native_static_libs(
                "native-static-libs: -lsupport -framework Echo -framework Wave \
                 -lsupport -framework Echo -lmath -lEcho"
            )
            .unwrap(),
            vec![
                NativeLib::Library("support".into()),
                NativeLib::Framework("Echo".into()),
                NativeLib::Framework("Wave".into()),
                NativeLib::Library("math".into()),
                NativeLib::Library("Echo".into()),
            ]
        );
    }

    #[test]
    fn unknown_native_library_tokens_are_refused() {
        for token in ["-pthread", "unexpected", "-l"] {
            let note = format!("native-static-libs: -lsupport {token}");
            let error = parse_native_static_libs(&note).unwrap_err().to_string();
            assert!(error.contains(token), "{error}");
        }
    }

    #[test]
    fn native_libraries_require_the_note_prefix_and_framework_name() {
        for (note, token) in [
            ("-lsupport", "-lsupport"),
            ("native-static-libs: -framework", "-framework"),
            ("native-static-libs: -framework -lsupport", "-lsupport"),
        ] {
            let error = parse_native_static_libs(note).unwrap_err().to_string();
            assert!(error.contains(token), "{error}");
        }
    }

    #[test]
    fn native_link_directives_stay_inside_the_module() {
        let libs = vec![
            NativeLib::Framework("Echo".into()),
            NativeLib::Library("support".into()),
        ];
        for modulemap in [
            "module Example {\n    header \"Example.h\"\n    export *\n}\n",
            "module Example {\n    header \"Example.h\"\n    export *\n  }\n",
            "module Example {\n    header \"Example.h\"\n    export * }\n",
        ] {
            assert_eq!(
                with_link_directives(modulemap, &libs).unwrap(),
                concat!(
                    "module Example {\n",
                    "    header \"Example.h\"\n",
                    "    export *\n",
                    "    link framework \"Echo\"\n",
                    "    link \"support\"\n",
                    "}\n",
                )
            );
        }
    }

    #[test]
    fn modulemaps_without_a_closing_brace_are_refused() {
        let error = with_link_directives("module Example {\n", &[])
            .unwrap_err()
            .to_string();
        assert!(error.contains('}'), "{error}");
    }

    #[test]
    fn fat_slices_link_the_ordered_union_of_their_targets() {
        let lists = vec![
            parse_native_static_libs("native-static-libs: -lsupport -framework Echo").unwrap(),
            parse_native_static_libs(
                "native-static-libs: -framework Wave -lsupport -framework Echo -lmath",
            )
            .unwrap(),
        ];
        assert_eq!(
            union_native_libs(&lists),
            vec![
                NativeLib::Library("support".into()),
                NativeLib::Framework("Echo".into()),
                NativeLib::Framework("Wave".into()),
                NativeLib::Library("math".into()),
            ]
        );
    }

    #[test]
    fn consumer_platform_maps_supported_plist_entries() {
        for (platform, variant, expected) in [
            ("ios", None, ("iphoneos", "ios", "")),
            (
                "ios",
                Some("simulator"),
                ("iphonesimulator", "ios", "-simulator"),
            ),
            ("macos", None, ("macosx", "macos", "")),
        ] {
            let mut library = PlistDictionary::new();
            library.insert("SupportedPlatform".into(), platform.into());
            if let Some(variant) = variant {
                library.insert("SupportedPlatformVariant".into(), variant.into());
            }
            assert_eq!(consumer_platform(&library).unwrap(), expected);
        }
    }

    #[test]
    fn consumer_platform_refuses_unknown_plist_platforms_and_variants() {
        for (platform, variant) in [
            ("unknown", None),
            ("ios", Some("unknown")),
            ("macos", Some("simulator")),
        ] {
            let mut library = PlistDictionary::new();
            library.insert("SupportedPlatform".into(), platform.into());
            if let Some(variant) = variant {
                library.insert("SupportedPlatformVariant".into(), variant.into());
            }
            assert!(consumer_platform(&library).is_err());
        }
    }

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
        let source =
            fs::read_to_string(FsPath::new(env!("CARGO_MANIFEST_DIR")).join("src/apple.rs"))
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
