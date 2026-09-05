use std::{env, fs, path::Path, process::Command, sync::LazyLock};

use anyhow::{Context, Result, bail};
use cargo_metadata::MetadataCommand;
use kithara_devtools::{Ctx, common::tools::ToolsConfig, util::check_tool};
use regex::Regex;

use crate::config::{KitharaExt, WasmConfig};

#[derive(Debug, clap::Subcommand)]
pub(crate) enum WasmCommand {
    /// Build WASM demo app via Trunk.
    Build {
        /// Build profile.
        #[arg(long, default_value_t = crate::BuildProfile::Release)]
        profile: crate::BuildProfile,
    },
    /// Build the bundle and weigh it against the budget it declares.
    SizeCheck {
        /// Build profile.
        #[arg(long, default_value_t = crate::BuildProfile::Release)]
        profile: crate::BuildProfile,
    },
    /// Trunk post-build hook: patch generated files for COEP compatibility.
    Postbuild {
        /// Trunk staging directory (defaults to `TRUNK_STAGING_DIR` env).
        #[arg(long, env = "TRUNK_STAGING_DIR")]
        staging_dir: String,
    },
}

pub(crate) fn run(cmd: WasmCommand, ctx: &Ctx) -> Result<()> {
    let ext = KitharaExt::from_ctx(ctx)?;
    let tools = &ctx.config.tools;
    match cmd {
        WasmCommand::Build { profile } => run_build(profile, tools),
        WasmCommand::SizeCheck { profile } => run_size_check(profile, tools),
        WasmCommand::Postbuild { staging_dir } => run_postbuild(&staging_dir, &ext.wasm),
    }
}

/// Which nightly builds the wasm bundle. The repository pins one, and CI
/// installs that exact name — asking for a toolchain called `nightly` there
/// fails, and `rustup` reports it the same way it reports its own absence.
fn nightly_toolchain() -> String {
    env::var("KITHARA_NIGHTLY_TOOLCHAIN").unwrap_or_else(|_| "nightly".to_string())
}

fn check_rust_target_nightly(target: &str) -> Result<bool> {
    let output = Command::new("rustup")
        .args(["target", "list", "--installed", "--toolchain"])
        .arg(nightly_toolchain())
        .output()
        .context("rustup target list")?;
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .any(|l| l == target))
}

fn check_rust_component_nightly(component: &str) -> Result<()> {
    let toolchain = nightly_toolchain();
    let output = Command::new("rustup")
        .args(["component", "list", "--installed", "--toolchain"])
        .arg(&toolchain)
        .output()
        .context("rustup component list")?;
    let installed = String::from_utf8_lossy(&output.stdout)
        .lines()
        .any(|l| l.starts_with(component));
    if !installed {
        bail!(
            "{component} not installed. Run: rustup component add {component} --toolchain {toolchain}"
        );
    }
    Ok(())
}

fn run_build(profile: crate::BuildProfile, tools: &ToolsConfig) -> Result<()> {
    let program = tools.program("trunk");
    check_tool(
        program,
        &["--version"],
        tools.install_hint("trunk", "cargo install trunk"),
    )?;
    let toolchain = nightly_toolchain();
    check_tool(
        "rustup",
        &["run", &toolchain, "rustc", "--version"],
        "rustup toolchain install nightly",
    )?;
    if !check_rust_target_nightly("wasm32-unknown-unknown")? {
        bail!(
            "wasm32-unknown-unknown not installed. \
             Run: rustup target add wasm32-unknown-unknown --toolchain nightly"
        );
    }
    check_rust_component_nightly("rust-src")?;

    let metadata = MetadataCommand::new().exec().context("cargo metadata")?;
    let root = metadata.workspace_root.as_std_path();
    let wasm_dir = root.join("crates/kithara-ffi");

    println!("==> Building kithara-ffi (wasm32)");
    let mut cmd = Command::new(program);
    cmd.args(["build", "--config", "Trunk.toml"]);
    if matches!(profile, crate::BuildProfile::Release) {
        cmd.arg("--release");
    }
    cmd.env("RUSTUP_TOOLCHAIN", &toolchain);
    cmd.current_dir(&wasm_dir);

    let status = cmd.status().context("failed to run trunk build")?;
    if !status.success() {
        bail!("trunk build failed");
    }

    println!("==> Done! Output in {}/dist/", wasm_dir.display());
    Ok(())
}

/// The budget in `.wasm-slim.toml`, which the file itself describes in terms of
/// the `dist` bundle.
#[derive(Debug, serde::Deserialize)]
struct SizeBudget {
    #[serde(rename = "target-size-kb")]
    target_kb: u64,
    #[serde(rename = "warn-threshold-kb")]
    warn_kb: u64,
    #[serde(rename = "max-size-kb")]
    max_kb: u64,
}

#[derive(Debug, serde::Deserialize)]
struct SlimConfig {
    #[serde(rename = "size-budget")]
    size_budget: SizeBudget,
}

/// Measure the bundle the browser downloads. `wasm-slim build --check` used to
/// do this, but it drives Cargo with the package's default features, and on
/// wasm this package must drop them — `uniffi` does not build for the target
/// and the crate declares it for every other one. Trunk already carries the
/// right selection in `index.html`, so the check builds what ships and weighs
/// that.
fn run_size_check(profile: crate::BuildProfile, tools: &ToolsConfig) -> Result<()> {
    run_build(profile, tools)?;

    let metadata = MetadataCommand::new().exec().context("cargo metadata")?;
    let root = metadata.workspace_root.as_std_path();
    let wasm_dir = root.join("crates/kithara-ffi");
    let config_path = wasm_dir.join(".wasm-slim.toml");
    let text = fs::read_to_string(&config_path)
        .with_context(|| format!("reading {}", config_path.display()))?;
    let config: SlimConfig =
        toml::from_str(&text).with_context(|| format!("parsing {}", config_path.display()))?;

    let dist = wasm_dir.join("dist");
    let bytes = directory_bytes(&dist)?;
    let kilobytes = bytes / 1024;

    let report = root.join("target/wasm-slim-result.json");
    if let Some(parent) = report.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    let json = format!(
        "{{\"bundle_kb\":{kilobytes},\"target_kb\":{},\"warn_kb\":{},\"max_kb\":{}}}\n",
        config.size_budget.target_kb, config.size_budget.warn_kb, config.size_budget.max_kb
    );
    fs::write(&report, json).with_context(|| format!("writing {}", report.display()))?;

    println!(
        "==> Bundle {kilobytes} KiB (target {}, warn {}, max {})",
        config.size_budget.target_kb, config.size_budget.warn_kb, config.size_budget.max_kb
    );
    if kilobytes > config.size_budget.max_kb {
        bail!(
            "wasm bundle is {kilobytes} KiB, over the {} KiB ceiling",
            config.size_budget.max_kb
        );
    }
    if kilobytes > config.size_budget.warn_kb {
        println!("==> Warning: past the warning threshold");
    }
    Ok(())
}

fn directory_bytes(path: &Path) -> Result<u64> {
    let mut total = 0;
    for entry in fs::read_dir(path).with_context(|| format!("reading {}", path.display()))? {
        let entry = entry.with_context(|| format!("reading {}", path.display()))?;
        let kind = entry
            .file_type()
            .context("reading a directory entry type")?;
        total += if kind.is_dir() {
            directory_bytes(&entry.path())?
        } else {
            entry.metadata().context("reading file size")?.len()
        };
    }
    Ok(total)
}

struct Consts;
impl Consts {
    const TEXT_DECODER_POLYFILL: &'static str = "\
if(typeof TextDecoder===\"undefined\"){\
globalThis.TextDecoder=class{constructor(){}\
decode(b){if(!b||!b.length)return\"\";let r=\"\";\
for(let i=0;i<b.length;i++)r+=String.fromCharCode(b[i]);return r}};\
globalThis.TextEncoder=class{constructor(){}\
encode(s){const a=new Uint8Array(s.length);\
for(let i=0;i<s.length;i++)a[i]=s.charCodeAt(i);return a}\
encodeInto(s,d){const e=this.encode(s);d.set(e);\
return{read:s.length,written:e.length}}}}\n";

    const BOOT_LOCK_FN: &'static str = "\
function __wstLockedInit(s,mod,mem,m){\
const bl=(m&&m.__wst_boot_lock_ptr)||0;\
if(bl>0){\
const li=new Int32Array(mem.buffer),lx=bl>>>2;\
while(Atomics.compareExchange(li,lx,0,1)!==0)Atomics.wait(li,lx,1,10);}\
try{s.initSync({module:mod,memory:mem,thread_stack_size:1048576});}\
finally{if(bl>0){\
const li2=new Int32Array(mem.buffer),lx2=bl>>>2;\
Atomics.store(li2,lx2,0);Atomics.notify(li2,lx2);}}}";

    const CHECK_RUNTIME_JS: &'static str = r#"

// --- Auto-generated by xtask wasm postbuild ---

/**
 * Check if the browser environment supports SharedArrayBuffer + COEP/COOP.
 * Returns { ok: boolean, reason?: string, waitingForReload?: boolean }.
 */
export function checkRuntime() {
    const crossOriginIsolated = self.crossOriginIsolated === true;
    const secureContext = self.isSecureContext === true;
    const sharedArrayBuffer = typeof SharedArrayBuffer !== 'undefined';

    if (secureContext && sharedArrayBuffer && crossOriginIsolated) {
        sessionStorage.removeItem('kithara_coi_reloaded');
        return { ok: true };
    }

    const waitingForReload =
        secureContext && !crossOriginIsolated &&
        typeof navigator.serviceWorker !== 'undefined' &&
        !navigator.serviceWorker.controller;

    if (waitingForReload) {
        navigator.serviceWorker.ready.then(() => {
            if (navigator.serviceWorker.controller || self.crossOriginIsolated === true) {
                sessionStorage.removeItem('kithara_coi_reloaded');
                return;
            }
            if (sessionStorage.getItem('kithara_coi_reloaded') === '1') return;
            sessionStorage.setItem('kithara_coi_reloaded', '1');
            window.location.reload();
        }).catch(() => {});
        return { ok: false, waitingForReload: true, reason: 'Waiting for COI service worker to activate' };
    }

    return {
        ok: false,
        waitingForReload: false,
        reason: `secureContext=${secureContext} crossOriginIsolated=${crossOriginIsolated} sharedArrayBuffer=${sharedArrayBuffer}`,
    };
}
"#;
}

mod wasm_patcher_regex {
    use super::*;
    pub(super) static RE_INTEGRITY: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#" integrity="[^"]*""#).expect("valid regex"));
    pub(super) static RE_CROSSORIGIN: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#" crossorigin="[^"]*""#).expect("valid regex"));
    pub(super) static RE_MODULEPRELOAD: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"<link rel="modulepreload"[^>]*>"#).expect("valid regex"));
    pub(super) static RE_PRELOAD: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"<link rel="preload"[^>]*>"#).expect("valid regex"));
}

fn strip_html_attrs(content: &str) -> String {
    let s = wasm_patcher_regex::RE_INTEGRITY.replace_all(content, "");
    let s = wasm_patcher_regex::RE_CROSSORIGIN.replace_all(&s, "");
    let s = wasm_patcher_regex::RE_MODULEPRELOAD.replace_all(&s, "");
    wasm_patcher_regex::RE_PRELOAD
        .replace_all(&s, "")
        .into_owned()
}

fn rewrite_paths(content: &str) -> String {
    content
        .replace("from '/kithara-ffi.js'", "from './kithara-ffi.js'")
        .replace(
            "module_or_path: '/kithara-ffi_bg.wasm'",
            "module_or_path: './kithara-ffi_bg.wasm'",
        )
}

fn apply_text_decoder_polyfill(content: &str) -> String {
    content.replace(
        "let cachedTextDecoder",
        &format!("{}let cachedTextDecoder", Consts::TEXT_DECODER_POLYFILL),
    )
}

fn apply_inline_patches(content: &str) -> String {
    content
        .replace(
            "__cleanup_handler(exitStatePtr);",
            "/* cleanup disabled: 8-byte leak per thread */",
        )
        .replace(
            "throw err;",
            "if (typeof close === \"function\") { close(); }",
        )
        .replace(
            "let __wstExitStatePtr = 0;",
            &format!("let __wstExitStatePtr = 0; {}", Consts::BOOT_LOCK_FN),
        )
        .replace(
            "shim.initSync({ module, memory, thread_stack_size: 1048576 });",
            "__wstLockedInit(shim, module, memory, meta);",
        )
        .replace(
            "__wst_parent_managed: true",
            "__wst_parent_managed: true, __wst_boot_lock_ptr: (globalThis.__wst_boot_lock_ptr || 0)",
        )
}

fn run_postbuild(staging_dir: &str, wasm: &WasmConfig) -> Result<()> {
    let dir = Path::new(staging_dir);
    anyhow::ensure!(dir.is_dir(), "staging dir does not exist: {staging_dir}");

    let js_name = &wasm.js_artifact;

    let index = dir.join("index.html");
    let content = fs::read_to_string(&index).context("read index.html")?;
    let content = strip_html_attrs(&content);
    let content = rewrite_paths(&content);
    fs::write(&index, content).context("write index.html")?;

    let js = dir.join(js_name);
    if js.exists() {
        let content = fs::read_to_string(&js).with_context(|| format!("read {js_name}"))?;
        let content = apply_text_decoder_polyfill(&content);
        let content = content + Consts::CHECK_RUNTIME_JS;
        fs::write(&js, content).with_context(|| format!("write {js_name}"))?;
        println!("post-build: {js_name} patched");
    }

    let pattern = format!("{}/snippets/wasm_safe_thread-*/inline0.js", dir.display());
    for entry in glob::glob(&pattern).context("glob inline0.js")? {
        let path = entry.context("glob entry")?;
        let content = fs::read_to_string(&path)?;
        let content = apply_inline_patches(&content);
        fs::write(&path, content)?;
        println!("post-build: patched {}", path.display());
    }

    println!("post-build: done");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_integrity_and_crossorigin_attrs() {
        let input = r#"<link rel="stylesheet" href="s.css" integrity="sha384-abc" crossorigin="anonymous">"#;
        let result = strip_html_attrs(input);
        assert_eq!(result, r#"<link rel="stylesheet" href="s.css">"#);
    }

    #[test]
    fn strip_modulepreload_and_preload_tags() {
        let input = r#"<link rel="modulepreload" href="foo.js"><p>keep</p><link rel="preload" href="bar.wasm" as="fetch">"#;
        let result = strip_html_attrs(input);
        assert_eq!(result, "<p>keep</p>");
    }

    #[test]
    fn rewrite_absolute_to_relative_paths() {
        let input = "from '/kithara-ffi.js'\nmodule_or_path: '/kithara-ffi_bg.wasm'";
        let result = rewrite_paths(input);
        assert_eq!(
            result,
            "from './kithara-ffi.js'\nmodule_or_path: './kithara-ffi_bg.wasm'"
        );
    }

    #[test]
    fn polyfill_text_decoder() {
        let input = "let cachedTextDecoder = new TextDecoder();";
        let result = apply_text_decoder_polyfill(input);
        assert!(result.starts_with("if(typeof TextDecoder===\"undefined\")"));
        assert!(result.contains("let cachedTextDecoder"));
    }

    #[test]
    fn disable_cleanup_handler() {
        let input = "something;\n__cleanup_handler(exitStatePtr);\nmore;";
        let result = apply_inline_patches(input);
        assert!(result.contains("/* cleanup disabled: 8-byte leak per thread */"));
        assert!(!result.contains("__cleanup_handler"));
    }

    #[test]
    fn fix_double_decrement() {
        let input = "catch(err) { postMessage(err); throw err; }";
        let result = apply_inline_patches(input);
        assert!(result.contains("if (typeof close === \"function\") { close(); }"));
        assert!(!result.contains("throw err;"));
    }

    #[test]
    fn inject_boot_lock() {
        let input = "let __wstExitStatePtr = 0;\nself.onmessage = ...";
        let result = apply_inline_patches(input);
        assert!(result.contains("function __wstLockedInit"));
    }

    #[test]
    fn replace_init_sync_with_locked() {
        let input = "shim.initSync({ module, memory, thread_stack_size: 1048576 });";
        let result = apply_inline_patches(input);
        assert!(result.contains("__wstLockedInit(shim, module, memory, meta);"));
    }

    #[test]
    fn pass_boot_lock_ptr_to_workers() {
        let input = "{ __wst_parent_managed: true }";
        let result = apply_inline_patches(input);
        assert!(result.contains("__wst_boot_lock_ptr: (globalThis.__wst_boot_lock_ptr || 0)"));
    }
}
