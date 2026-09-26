//! Embed `app.yaml` verbatim and, beside it, the values its `$KITHARA_...`
//! references resolved to at build time. The application parses the same text
//! at startup, so the build decides nothing about what a field means.
//!
//! Reference resolution order for a given name, at run time:
//! 1. The process environment.
//! 2. This table, emitted as `crate::baked::baked_env`.
//! 3. Nothing — the application refuses to start and names what is unset.
//!
//! Values read from an environment reference are emitted as `obfstr!("…")` so
//! a shipped secret is not a plain run of bytes in `strings` output.
//!
//! Lanes that build against a real key server set `KITHARA_DRM_REQUIRE` (any
//! non-empty value): an upfront pass then validates every reference in
//! `app.yaml` and fails the build listing all missing variables.
//!
//! `app.yaml`, `app.web.yaml` and an existing `.env` are `rerun-if-changed`.

use std::{
    collections::HashMap,
    env,
    fmt::Write,
    fs,
    path::{Path, PathBuf},
};

#[path = "src/document/bake.rs"]
mod bake;
#[path = "src/document/merge.rs"]
mod merge;

fn main() {
    let manifest_dir =
        PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set by cargo"));
    let app_yaml_path = manifest_dir.join("app.yaml");
    let workspace_root = manifest_dir.join("..").join("..");
    let dotenv_path = workspace_root.join(".env");

    println!("cargo:rerun-if-changed={}", app_yaml_path.display());
    // Cargo treats a missing watched path as dirty on every build.
    if dotenv_path.is_file() {
        println!("cargo:rerun-if-changed={}", dotenv_path.display());
    }

    let web_yaml_path = manifest_dir.join("app.web.yaml");
    println!("cargo:rerun-if-changed={}", web_yaml_path.display());
    let yaml_src = fs::read_to_string(&app_yaml_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", app_yaml_path.display()));
    let web_src = fs::read_to_string(&web_yaml_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", web_yaml_path.display()));

    let env_map = load_env(&dotenv_path);
    let target_arch =
        env::var("CARGO_CFG_TARGET_ARCH").expect("CARGO_CFG_TARGET_ARCH is set by cargo");
    let baked = bake::bake(&target_arch, &yaml_src, &web_src, &env_map)
        .unwrap_or_else(|e| panic!("bake the configuration document: {e}"));
    println!("cargo:rerun-if-env-changed=KITHARA_DRM_REQUIRE");

    for (_, name) in &baked.refs {
        println!("cargo:rerun-if-env-changed={name}");
    }
    if env_map
        .get("KITHARA_DRM_REQUIRE")
        .is_some_and(|v| !v.is_empty())
    {
        let missing: Vec<String> = baked
            .refs
            .iter()
            .filter(|(_, name)| env_map.get(name).is_none_or(String::is_empty))
            .map(|(label, name)| format!("{label}: `{name}`"))
            .collect();
        if !missing.is_empty() {
            panic!(
                "KITHARA_DRM_REQUIRE is set but these env vars are unset or empty:\n  {}",
                missing.join("\n  ")
            );
        }
    }

    let mut code = String::new();
    emit_document(&mut code, &baked.document);
    emit_env_table(&mut code, &baked.resolved);

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR is set by cargo"));
    let out_path = out_dir.join("app_config_baked.rs");
    fs::write(&out_path, code).unwrap_or_else(|e| panic!("write {}: {e}", out_path.display()));

    emit_ui_documents(&manifest_dir, &out_dir);
}

/// Embed the document verbatim. The application parses this same text at
/// startup, so the build no longer decides what any field means.
fn emit_document(code: &mut String, yaml_src: &str) {
    writeln!(
        code,
        "pub(crate) const BAKED_DOCUMENT: &str = {yaml_src:?};"
    )
    .expect("write to String never fails");
}

/// Emit the second place a reference is resolved from. The table carries only
/// the names this build found a value for and answers `None` to everything
/// else, so a name it had nothing for reaches the startup unresolved and is
/// named there. Values are wrapped with `obfstr!()` so a shipped secret is not
/// a plain run of bytes in `strings` output.
fn emit_env_table(code: &mut String, resolved: &[(String, String)]) {
    // A build that resolved nothing -- every lane without credentials -- has no
    // table to match against, and a `match` left with one wildcard arm is not a
    // table either.
    if resolved.is_empty() {
        code.push_str(
            "#[must_use]\npub(crate) fn baked_env(_name: &str) -> Option<String> {\n    None\n}\n",
        );
        return;
    }

    code.push_str(
        "#[must_use]\npub(crate) fn baked_env(name: &str) -> Option<String> {\n    match name {\n",
    );
    for (name, value) in resolved {
        writeln!(
            code,
            "        {name:?} => Some(::obfstr::obfstr!({value:?}).to_string()),"
        )
        .expect("write to String never fails");
    }
    code.push_str("        _ => None,\n    }\n}\n");
}

/// Embed this application's own UI folder, read from the folder itself.
///
/// A document added to the folder ships by that alone, so the screens the
/// application draws and the files it is written in cannot drift apart. A
/// package laid out on disk still wins over these when the application runs.
fn emit_ui_documents(manifest_dir: &Path, out_dir: &Path) {
    let root = manifest_dir.join("assets").join("ui");
    println!("cargo:rerun-if-changed={}", root.display());

    let mut documents = Vec::new();
    collect_documents(&root, &root, &mut documents);
    documents.sort();

    let mut code = String::from("const DOCS: &[(&str, &str)] = &[\n");
    for (named, read) in &documents {
        writeln!(code, "    ({named:?}, include_str!({read:?})),")
            .expect("write to String never fails");
    }
    code.push_str("];\n");

    let out_path = out_dir.join("ui_documents.rs");
    fs::write(&out_path, code).unwrap_or_else(|e| panic!("write {}: {e}", out_path.display()));
}

/// Every document under `dir`, as the path a package names it by and the path
/// the build reads it from.
fn collect_documents(root: &Path, dir: &Path, out: &mut Vec<(String, String)>) {
    let entries = fs::read_dir(dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display()));
    for entry in entries {
        let path = entry
            .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
            .path();
        if path.is_dir() {
            collect_documents(root, &path, out);
            continue;
        }
        if path.extension().is_none_or(|extension| extension != "ron") {
            continue;
        }
        let named = path
            .strip_prefix(root)
            .expect("every file walked sits under the folder")
            .to_str()
            .expect("the shipped folder holds no unnameable path")
            .replace('\\', "/");
        let read = path
            .to_str()
            .expect("the checkout holds no unnameable path")
            .to_owned();
        out.push((named, read));
    }
}

fn load_env(path: &PathBuf) -> HashMap<String, String> {
    let mut map: HashMap<String, String> = env::vars().collect();
    let Ok(contents) = fs::read_to_string(path) else {
        return map;
    };
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some((key, value)) = trimmed.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if !key.starts_with("KITHARA_") {
            continue;
        }
        if map.contains_key(key) {
            continue;
        }
        let value = value.trim().trim_matches(|c: char| c == '"' || c == '\'');
        map.insert(key.to_string(), value.to_string());
    }
    map
}
