use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

use anyhow::{Context as _, Result, ensure};
use kithara_devtools::Ctx;
use quote::ToTokens as _;
use serde_json::json;

use crate::{
    child::{self, Cancel},
    ci::CiPins,
};

const WASM_RUSTFLAGS: &str = "--cfg=web_sys_unstable_apis -C target-feature=+atomics,+bulk-memory,+mutable-globals -C link-arg=--shared-memory -C link-arg=--import-memory -C link-arg=--max-memory=67108864 -C link-arg=--export=__wasm_init_tls -C link-arg=--export=__tls_size -C link-arg=--export=__tls_align -C link-arg=--export=__tls_base -C link-arg=--export=__heap_base -C panic=abort";

#[derive(Debug, clap::Args)]
pub(crate) struct Args {
    /// Clean checkout of the pinned uniffi-bindgen-react-native revision.
    #[arg(long)]
    backend: PathBuf,
}

pub(super) fn run(args: Args, ctx: &Ctx) -> Result<()> {
    let cancel = Cancel::install()?;
    let pins = CiPins::load(&ctx.root.join(".config/ci-pins.toml"))?;
    let backend_pin = backend_pin(&ctx.root)?;
    let backend = ctx.root.join(args.backend).canonicalize()?;
    validate_tools(&backend, &backend_pin, &pins, &cancel)?;
    let output = ctx.root.join("target/config-protocol/sdk");
    let generated = output.join("generated");
    let shim = output.join("shim");
    fs::create_dir_all(shim.join("src"))?;
    build_backend(&backend, &backend.join("target"), &cancel)?;
    let native = output.join("native");
    run_command(
        Command::new("cargo")
            .current_dir(&ctx.root)
            .args(["build", "--locked", "-p", "kithara-config-uniffi-probe"])
            .env("CARGO_TARGET_DIR", &native),
        &cancel,
    )?;
    let library = native.join("debug").join(format!(
        "{}kithara_config_uniffi_probe{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    ));
    generate_bindings(&backend, &generated, &shim, &library, None, &cancel)?;
    let module = shim.join("src/kithara_config_uniffi_probe_module.rs");
    fs::write(&module, canonical_rust(&fs::read_to_string(&module)?)?)?;
    write_shim(&ctx.root, &shim, &backend)?;
    fs::copy(
        ctx.root.join("tests/crates/ffi-sdk/wasm.lock"),
        shim.join("Cargo.lock"),
    )?;
    build_wasm(&shim, &pins.nightly_toolchain, &cancel)?;
    run_command(
        Command::new("wasm-bindgen")
            .arg(shim.join("target/wasm32-unknown-unknown/debug/kithara_config_web_probe.wasm"))
            .args(["--target", "web", "--out-name", "index", "--out-dir"])
            .arg(generated.join("wasm-bindgen")),
        &cancel,
    )?;
    bundle(&ctx.root, &output, &backend, &cancel)?;
    tracing::info!(directory = %output.display(), revision = %backend_pin.rev, "generated SDK fixture ready");
    Ok(())
}

pub(super) fn run_product(args: Args, ctx: &Ctx) -> Result<()> {
    let cancel = Cancel::install()?;
    let pins = CiPins::load(&ctx.root.join(".config/ci-pins.toml"))?;
    let backend_pin = backend_pin(&ctx.root)?;
    let backend = ctx.root.join(args.backend).canonicalize()?;
    validate_tools(&backend, &backend_pin, &pins, &cancel)?;
    let output = ctx.root.join("target/config-protocol/product-sdk");
    let generated = output.join("generated");
    let shim = output.join("shim");
    fs::create_dir_all(shim.join("src"))?;
    build_backend(&backend, &output.join("backend-target"), &cancel)?;
    build_product_metadata_wasm(&ctx.root, &shim, &pins.nightly_toolchain, &cancel)?;
    let library = shim.join("target/wasm32-unknown-unknown/debug/kithara_ffi.wasm");
    generate_bindings(
        &backend,
        &generated,
        &shim,
        &library,
        Some(&output.join("backend-target")),
        &cancel,
    )?;
    let module = shim.join("src/kithara_ffi_module.rs");
    fs::write(&module, canonical_rust(&fs::read_to_string(&module)?)?)?;
    write_product_shim(&ctx.root, &shim, &backend)?;
    fs::copy(
        ctx.root.join("tests/crates/ffi-web/product/wasm.lock"),
        shim.join("Cargo.lock"),
    )?;
    build_wasm(&shim, &pins.nightly_toolchain, &cancel)?;
    run_command(
        Command::new("wasm-bindgen")
            .arg(
                shim.join(
                    "target/wasm32-unknown-unknown/debug/kithara_product_web_uniffi_probe.wasm",
                ),
            )
            .args(["--target", "web", "--out-name", "kithara-ffi", "--out-dir"])
            .arg(generated.join("wasm-bindgen")),
        &cancel,
    )?;
    bundle_product(&ctx.root, &output, &backend, &cancel)?;
    tracing::info!(directory = %output.display(), "product UniFFI Web host SDK ready");
    Ok(())
}

fn generate_bindings(
    backend: &Path,
    generated: &Path,
    shim: &Path,
    library: &Path,
    product_target: Option<&Path>,
    cancel: &Cancel,
) -> Result<()> {
    let target = product_target.map_or_else(|| backend.join("target"), Path::to_path_buf);
    let generator = target.join(format!(
        "debug/uniffi-bindgen-react-native{}",
        std::env::consts::EXE_SUFFIX
    ));
    run_command(
        Command::new(generator)
            .args([
                "generate",
                "bindings",
                "--library",
                "--flavor",
                "wasm",
                "--no-format",
                "--ts-dir",
            ])
            .arg(generated)
            .arg("--cpp-dir")
            .arg(shim.join("src"))
            .arg(library),
        cancel,
    )
}

struct BackendPin {
    git: String,
    rev: String,
}

fn backend_pin(root: &Path) -> Result<BackendPin> {
    let manifest: toml::Value = toml::from_str(&fs::read_to_string(root.join("Cargo.toml"))?)?;
    let metadata = manifest
        .get("workspace")
        .and_then(|value| value.get("metadata"))
        .and_then(|value| value.get("uniffi-javascript"))
        .context("missing [workspace.metadata.uniffi-javascript] in Cargo.toml")?;
    let git = metadata
        .get("git")
        .and_then(toml::Value::as_str)
        .context("UniFFI Web backend git URL")?
        .to_owned();
    ensure!(
        github_repository(&git).is_some(),
        "UniFFI Web backend git URL must name a GitHub repository"
    );
    let rev = metadata
        .get("rev")
        .and_then(toml::Value::as_str)
        .context("UniFFI Web backend revision")?
        .to_owned();
    ensure!(
        rev.len() == 40 && rev.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "UniFFI Web backend revision must be a full Git commit hash"
    );
    Ok(BackendPin { git, rev })
}

fn github_repository(url: &str) -> Option<&str> {
    let url = url.trim().trim_end_matches('/');
    let url = url.strip_suffix(".git").unwrap_or(url);
    url.strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("git@github.com:"))
        .or_else(|| url.strip_prefix("ssh://git@github.com/"))
        .or_else(|| url.strip_prefix("ssh://git@ssh.github.com:443/"))
        .filter(|repo| {
            repo.split_once('/').is_some_and(|(owner, name)| {
                !owner.is_empty() && !name.is_empty() && !name.contains('/')
            })
        })
}

fn validate_tools(
    backend: &Path,
    backend_pin: &BackendPin,
    pins: &CiPins,
    cancel: &Cancel,
) -> Result<()> {
    let revision = capture(
        Command::new("git")
            .arg("-C")
            .arg(backend)
            .args(["rev-parse", "HEAD"]),
        cancel,
    )?;
    ensure!(
        revision.trim() == backend_pin.rev,
        "SDK backend revision differs from Cargo.toml pin {} at {}",
        backend_pin.rev,
        backend_pin.git
    );
    let remote = capture(
        Command::new("git")
            .arg("-C")
            .arg(backend)
            .args(["remote", "get-url", "origin"]),
        cancel,
    )?;
    let expected_repo = github_repository(&backend_pin.git).context("backend pin git URL")?;
    ensure!(
        github_repository(&remote).is_some_and(|repo| repo.eq_ignore_ascii_case(expected_repo)),
        "SDK backend origin {} differs from Cargo.toml pin {}",
        remote.trim(),
        backend_pin.git
    );
    let dirty = capture(
        Command::new("git")
            .arg("-C")
            .arg(backend)
            .args(["status", "--porcelain"]),
        cancel,
    )?;
    ensure!(dirty.is_empty(), "SDK backend is not clean");
    let bun = capture(Command::new("bun").arg("--version"), cancel)?;
    ensure!(
        bun.trim() == pins.bun_version,
        "Bun version differs from CI pin"
    );
    let bindgen = capture(Command::new("wasm-bindgen").arg("--version"), cancel)?;
    let expected = pins
        .cargo_tools
        .get("wasm-bindgen-cli")
        .context("wasm-bindgen-cli pin")?;
    ensure!(
        bindgen.trim() == format!("wasm-bindgen {expected}"),
        "wasm-bindgen version differs from CI pin"
    );
    Ok(())
}

/// Sorts the backend's unordered callback modules, preserving ABI field order.
fn canonical_rust(source: &str) -> Result<String> {
    let mut file = syn::parse_file(source)?;
    file.items.sort_by_cached_key(|item| match item {
        syn::Item::Mod(module) => (true, module.ident.to_string()),
        _ => (false, String::new()),
    });
    Ok(file.into_token_stream().to_string())
}

fn capture(command: &mut Command, cancel: &Cancel) -> Result<String> {
    let output = child::output(command, Some(cancel), Duration::from_secs(30))?;
    ensure!(
        output.status.success(),
        "SDK tool failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).context("SDK tool output encoding")
}

fn run_command(command: &mut Command, cancel: &Cancel) -> Result<()> {
    let status = child::run(command.env("CARGO_BUILD_JOBS", "2"), Some(cancel))?;
    ensure!(status.success(), "SDK build step failed: {status}");
    Ok(())
}

fn build_backend(backend: &Path, target: &Path, cancel: &Cancel) -> Result<()> {
    run_command(
        Command::new("cargo")
            .current_dir(std::env::temp_dir())
            .args(["build", "--locked", "-p", "uniffi-bindgen-react-native"])
            .arg("--manifest-path")
            .arg(backend.join("Cargo.toml"))
            .env("CARGO_TARGET_DIR", target),
        cancel,
    )
}

fn write_shim(root: &Path, shim: &Path, backend: &Path) -> Result<()> {
    let workspace: toml::Value = toml::from_str(&fs::read_to_string(root.join("Cargo.toml"))?)?;
    let wasm_bindgen = workspace
        .get("workspace")
        .and_then(|value| value.get("dependencies"))
        .and_then(|value| value.get("wasm-bindgen"))
        .context("workspace wasm-bindgen dependency")?
        .clone();
    let mut manifest: toml::Value = toml::from_str(
        r#"
[package]
name = "kithara-config-web-probe"
version = "0.0.0"
edition = "2018"
publish = false
[workspace]
[lib]
crate-type = ["cdylib"]
[dependencies]
kithara-config-uniffi-probe = { path = "", features = ["single-threaded"] }
uniffi-runtime-javascript = { path = "", features = ["wasm32"] }
"#,
    )?;
    manifest["dependencies"]["kithara-config-uniffi-probe"]["path"] = toml::Value::String(
        root.join("tests/crates/ffi-sdk")
            .to_string_lossy()
            .into_owned(),
    );
    manifest["dependencies"]["uniffi-runtime-javascript"]["path"] = toml::Value::String(
        backend
            .join("crates/uniffi-runtime-javascript")
            .to_string_lossy()
            .into_owned(),
    );
    manifest["dependencies"]
        .as_table_mut()
        .context("shim dependencies")?
        .insert("wasm-bindgen".into(), wasm_bindgen);
    fs::write(
        shim.join("src/lib.rs"),
        "use kithara_config_uniffi_probe as _;\nmod kithara_config_uniffi_probe_module;\n",
    )?;
    fs::write(shim.join("Cargo.toml"), toml::to_string_pretty(&manifest)?)?;
    Ok(())
}

fn write_product_shim(root: &Path, shim: &Path, backend: &Path) -> Result<()> {
    let workspace: toml::Value = toml::from_str(&fs::read_to_string(root.join("Cargo.toml"))?)?;
    let mut manifest: toml::Value = toml::from_str(
        r#"
[package]
name = "kithara-product-web-uniffi-probe"
version = "0.0.0"
edition = "2018"
publish = false
[workspace]
resolver = "2"
[lib]
crate-type = ["cdylib"]
[dependencies]
kithara-ffi = { path = "", default-features = false, features = ["wasm", "uniffi-web"] }
uniffi-runtime-javascript = { path = "", features = ["wasm32"] }
"#,
    )?;
    manifest["dependencies"]["kithara-ffi"]["path"] = toml::Value::String(
        root.join("crates/kithara-ffi")
            .to_string_lossy()
            .into_owned(),
    );
    manifest["dependencies"]["uniffi-runtime-javascript"]["path"] = toml::Value::String(
        backend
            .join("crates/uniffi-runtime-javascript")
            .to_string_lossy()
            .into_owned(),
    );
    manifest["dependencies"]
        .as_table_mut()
        .context("product shim dependencies")?
        .insert(
            "wasm-bindgen".into(),
            workspace["workspace"]["dependencies"]["wasm-bindgen"].clone(),
        );
    fs::write(
        shim.join("src/lib.rs"),
        "use kithara_ffi as _;\nmod kithara_ffi_module;\n",
    )?;
    fs::write(shim.join("Cargo.toml"), toml::to_string_pretty(&manifest)?)?;
    Ok(())
}

fn build_wasm(shim: &Path, nightly: &str, cancel: &Cancel) -> Result<()> {
    run_command(
        Command::new("cargo")
            .current_dir(shim)
            .arg(format!("+{nightly}"))
            .args([
                "build",
                "--locked",
                "--target",
                "wasm32-unknown-unknown",
                "-Z",
                "build-std=std,panic_abort",
            ])
            .env("CARGO_TARGET_DIR", shim.join("target"))
            .env("RUSTFLAGS", WASM_RUSTFLAGS),
        cancel,
    )
}

fn build_product_metadata_wasm(
    root: &Path,
    shim: &Path,
    nightly: &str,
    cancel: &Cancel,
) -> Result<()> {
    run_command(
        Command::new("cargo")
            .current_dir(root)
            .arg(format!("+{nightly}"))
            .args([
                "build",
                "--locked",
                "--target",
                "wasm32-unknown-unknown",
                "-Z",
                "build-std=std,panic_abort",
                "-p",
                "kithara-ffi",
                "--lib",
                "--no-default-features",
                "--features",
                "wasm,uniffi-web,symphonia,client-reqwest,tls-rustls",
            ])
            .env("CARGO_TARGET_DIR", shim.join("target"))
            .env("RUSTFLAGS", WASM_RUSTFLAGS),
        cancel,
    )
}

fn bundle(root: &Path, output: &Path, backend: &Path, cancel: &Cancel) -> Result<()> {
    for file in [
        "contract.ts",
        "browser.ts",
        "worker.ts",
        "wake-worker.ts",
        "index.html",
    ] {
        fs::copy(
            root.join("tests/crates/ffi-web/sdk").join(file),
            output.join(file),
        )?;
    }
    let config = json!({ "compilerOptions": {
        "paths": { "@ubjs/core": [backend.join("typescript/src/index.ts")] },
        "target": "ES2022", "module": "ESNext", "moduleResolution": "Bundler",
        "strict": true, "noEmit": true, "allowImportingTsExtensions": true,
        "lib": ["ES2022", "ESNext.Disposable", "DOM"], "types": []
    }, "include": ["*.ts", "generated/**/*.ts"] });
    fs::write(
        output.join("tsconfig.json"),
        serde_json::to_string_pretty(&config)?,
    )?;
    run_command(
        Command::new("npm")
            .current_dir(backend.join("typescript"))
            .args(["ci", "--ignore-scripts", "--no-audit", "--no-fund"]),
        cancel,
    )?;
    run_command(
        Command::new("node")
            .arg(backend.join("typescript/node_modules/typescript/bin/tsc"))
            .arg("--project")
            .arg(output.join("tsconfig.json")),
        cancel,
    )?;
    run_command(
        Command::new("bun").current_dir(output).args([
            "build",
            "browser.ts",
            "worker.ts",
            "wake-worker.ts",
            "--outdir",
            ".",
            "--target",
            "browser",
        ]),
        cancel,
    )
}

fn bundle_product(root: &Path, output: &Path, backend: &Path, cancel: &Cancel) -> Result<()> {
    for file in ["browser.ts", "index.html"] {
        fs::copy(
            root.join("tests/crates/ffi-web/product").join(file),
            output.join(file),
        )?;
    }
    let generated = output.join("generated/kithara_ffi.ts");
    let source = fs::read_to_string(&generated)?;
    let original = "import * as wasmBundle from \"./wasm-bindgen/index.js\";";
    ensure!(
        source.matches(original).count() == 1,
        "product UniFFI generated Wasm import changed"
    );
    fs::write(
        &generated,
        source.replace(
            original,
            "const wasmBundle = await import(new URL(\"./generated/wasm-bindgen/kithara-ffi.js\", import.meta.url).href);",
        ),
    )?;
    let config = json!({ "compilerOptions": {
        "paths": { "@ubjs/core": [backend.join("typescript/src/index.ts")] },
        "target": "ES2022", "module": "ESNext", "moduleResolution": "Bundler",
        "strict": true, "noEmit": true, "allowImportingTsExtensions": true,
        "lib": ["ES2022", "ESNext.Disposable", "DOM"], "types": []
    }, "include": ["*.ts", "generated/**/*.ts"] });
    fs::write(
        output.join("tsconfig.json"),
        serde_json::to_string_pretty(&config)?,
    )?;
    run_command(
        Command::new("npm")
            .current_dir(backend.join("typescript"))
            .args(["ci", "--ignore-scripts", "--no-audit", "--no-fund"]),
        cancel,
    )?;
    run_command(
        Command::new("node")
            .arg(backend.join("typescript/node_modules/typescript/bin/tsc"))
            .arg("--project")
            .arg(output.join("tsconfig.json")),
        cancel,
    )?;
    run_command(
        Command::new("bun").current_dir(output).args([
            "build",
            "browser.ts",
            "--outdir",
            ".",
            "--target",
            "browser",
        ]),
        cancel,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_origin_accepts_https_and_ssh_for_the_same_repository() {
        let https = "https://github.com/gerasim13/uniffi-bindgen-react-native";
        let ssh = "git@github.com:gerasim13/uniffi-bindgen-react-native.git";
        let ssh_url = "ssh://git@github.com/gerasim13/uniffi-bindgen-react-native.git";
        let ssh_443 = "ssh://git@ssh.github.com:443/gerasim13/uniffi-bindgen-react-native.git";
        assert_eq!(github_repository(https), github_repository(ssh));
        assert_eq!(github_repository(https), github_repository(ssh_url));
        assert_eq!(github_repository(https), github_repository(ssh_443));
        assert_ne!(
            github_repository(https),
            github_repository("https://github.com/other/uniffi-bindgen-react-native")
        );
        assert_eq!(github_repository("https://github.com//repo"), None);
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let pin = backend_pin(root).unwrap();
        assert_eq!(
            github_repository(&pin.git),
            Some("gerasim13/uniffi-bindgen-react-native")
        );
    }

    #[test]
    fn canonical_modules_keep_abi_fields_in_declared_order() {
        let first = "#[repr(C)] struct Layout { z: u64, a: u32 } mod z {} mod a {}";
        let reordered = "#[repr(C)] struct Layout { z: u64, a: u32 } mod a {} mod z {}";
        let different_abi = "#[repr(C)] struct Layout { a: u32, z: u64 } mod a {} mod z {}";
        assert_eq!(
            canonical_rust(first).unwrap(),
            canonical_rust(reordered).unwrap()
        );
        assert_ne!(
            canonical_rust(first).unwrap(),
            canonical_rust(different_abi).unwrap()
        );
    }

    #[test]
    fn shim_generation_emits_a_crate_root_and_uses_workspace_dependencies() {
        let root = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join("Cargo.toml"),
            "[workspace.dependencies]\nwasm-bindgen = { version = '=9.8.7', default-features = false }\n",
        )
        .unwrap();
        let shim = root.path().join("shim");
        fs::create_dir_all(shim.join("src")).unwrap();
        write_shim(root.path(), &shim, &root.path().join("backend")).unwrap();
        let source = fs::read_to_string(shim.join("src/lib.rs")).unwrap();
        assert!(syn::parse_file(&source).is_ok());
        assert!(source.contains("mod kithara_config_uniffi_probe_module;"));
        let manifest: toml::Value =
            toml::from_str(&fs::read_to_string(shim.join("Cargo.toml")).unwrap()).unwrap();
        assert_eq!(
            manifest["dependencies"]["wasm-bindgen"]["version"].as_str(),
            Some("=9.8.7")
        );
        assert_eq!(
            manifest["dependencies"]["wasm-bindgen"]["default-features"].as_bool(),
            Some(false)
        );
    }
}
