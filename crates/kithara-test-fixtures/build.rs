//! Materializes every registered asset into the shared store and writes one
//! accessor per case into `OUT_DIR`.
//!
//! `src/defs/` and `src/registry.rs` compile only here — the library never sees
//! them, which is what keeps encoding itself out of the target build.
//! `src/signal/` and `src/store/disk.rs` are shared: the same sources, reached
//! from two roots.

#[path = "src/context.rs"]
mod context;
#[path = "src/defs/mod.rs"]
mod defs;
#[path = "src/registry.rs"]
mod registry;
#[path = "src/remote_file.rs"]
mod remote_file;
#[path = "src/variant_input.rs"]
pub mod variant_input;
// `fmp4`, `signal`, and `store` keep the visibility they have in the library:
// the same source files, reached from two roots.
#[path = "src/fmp4/mod.rs"]
pub mod fmp4;
#[path = "src/graph.rs"]
mod graph;
#[path = "src/hls/hydrate.rs"]
mod hls_hydrate;
#[path = "src/hls/manifest.rs"]
mod hls_manifest;
#[path = "src/signal/mod.rs"]
pub mod signal;
#[path = "src/store/disk.rs"]
pub mod store;

use std::{
    collections::{HashMap, HashSet},
    fmt::Write as _,
    fs,
    panic::resume_unwind,
    path::{Path, PathBuf},
    thread,
};

use registry::{AssetBuild, AssetDef};

use self::context::BuildContext;

const REMOTE_FIXTURES_ENV: &str = "KITHARA_REMOTE_FIXTURES";

/// Rejects two cases that would produce one accessor, before either is written.
fn resolve(defs: &[&'static AssetDef]) -> Vec<(String, String, &'static AssetDef)> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut resolved = Vec::with_capacity(defs.len());
    for def in defs {
        let name = def.accessor_name();
        assert!(
            seen.insert(name.clone()),
            "kithara-test-fixtures: two asset cases share the accessor name `{name}`",
        );
        let id = store::asset_id(def.func, def.case);
        resolved.push((name, id, *def));
    }
    resolved.sort_by(|(left, _, _), (right, _, _)| left.cmp(right));
    resolved
}

fn materialize(
    namespace: &Path,
    resolved: &[(String, String, &'static AssetDef)],
) -> HashMap<String, String> {
    let mut unavailable = HashMap::new();
    let nodes: Vec<_> = resolved
        .iter()
        .map(|(name, _, def)| graph::Node {
            name,
            dependencies: def.dependencies,
        })
        .collect();
    let levels =
        graph::levels(&nodes).unwrap_or_else(|error| panic!("kithara-test-fixtures: {error}"));
    let parallelism = thread::available_parallelism().map_or(1, usize::from);

    for level in levels {
        for batch in level.chunks(parallelism) {
            let unavailable_snapshot = &unavailable;
            let results = thread::scope(|scope| {
                batch
                    .iter()
                    .map(|&index| {
                        scope.spawn(move || {
                            materialize_one(namespace, resolved, index, unavailable_snapshot)
                        })
                    })
                    .collect::<Vec<_>>()
                    .into_iter()
                    .map(|handle| handle.join().unwrap_or_else(|panic| resume_unwind(panic)))
                    .collect::<Vec<_>>()
            });
            unavailable.extend(results.into_iter().flatten());
        }
    }
    unavailable
}

fn materialize_one(
    namespace: &Path,
    resolved: &[(String, String, &'static AssetDef)],
    index: usize,
    unavailable: &HashMap<String, String>,
) -> Option<(String, String)> {
    let (name, id, def) = &resolved[index];
    if store::has_entry(namespace, id, def.ext) {
        return None;
    }
    if def.optional && std::env::var_os(REMOTE_FIXTURES_ENV).is_none() {
        return Some((
            name.clone(),
            format!("remote hydration disabled; set {REMOTE_FIXTURES_ENV}"),
        ));
    }
    let _lock = store::lock_entry(namespace, id)
        .unwrap_or_else(|error| panic!("kithara-test-fixtures: lock for `{name}`: {error}"));
    if store::has_entry(namespace, id, def.ext) {
        return None;
    }
    if let Some((dependency, reason)) = def.dependencies.iter().find_map(|dependency| {
        unavailable
            .get(*dependency)
            .map(|reason| (dependency, reason))
    }) {
        if def.optional {
            return Some((
                name.clone(),
                format!("dependency `{dependency}` unavailable: {reason}"),
            ));
        }
        panic!(
            "kithara-test-fixtures: required fixture `{name}` depends on unavailable `{dependency}`: {reason}"
        );
    }
    let dependency_bytes: Vec<_> = def
        .dependencies
        .iter()
        .map(|dependency| {
            let (_, dependency_id, dependency_def) = resolved
                .iter()
                .find(|(candidate, _, _)| candidate == dependency)
                .unwrap_or_else(|| {
                    panic!(
                        "kithara-test-fixtures: graph accepted missing dependency `{dependency}`"
                    )
                });
            store::read_entry(namespace, dependency_id, dependency_def.ext).unwrap_or_else(|| {
                panic!(
                    "kithara-test-fixtures: dependency `{dependency}` of `{name}` was not materialized"
                )
            })
        })
        .collect();
    let inputs: Vec<_> = dependency_bytes.iter().map(Vec::as_slice).collect();
    let context = BuildContext::new(namespace, id);
    let bytes = match (def.build)(context, &inputs) {
        AssetBuild::Ready(bytes) => bytes,
        AssetBuild::Unavailable(reason) if def.optional => {
            println!("cargo:warning=optional fixture `{name}` unavailable: {reason}");
            return Some((name.clone(), reason));
        }
        AssetBuild::Unavailable(reason) => {
            panic!("kithara-test-fixtures: required fixture `{name}` unavailable: {reason}")
        }
    };
    assert!(
        !bytes.is_empty(),
        "kithara-test-fixtures: `{name}` produced no bytes"
    );
    store::write_entry(namespace, id, def.ext, &bytes)
        .unwrap_or_else(|error| panic!("kithara-test-fixtures: write `{name}`: {error}"));
    None
}

fn codegen(
    namespace: &Path,
    resolved: &[(String, String, &'static AssetDef)],
    unavailable: &HashMap<String, String>,
    embed_assets: bool,
) -> String {
    let mut out = String::new();
    let mut manifest = String::new();
    let mut by_name = String::new();
    for (name, id, def) in resolved {
        let entry = format!("ENTRY_{}", name.to_uppercase());
        let path = store::entry_path(namespace, id, def.ext);
        let path = path.to_str().unwrap_or_else(|| {
            panic!("kithara-test-fixtures: the store path for `{name}` is not valid UTF-8")
        });
        let relative_path = format!("{}/{id}.{}", store::CACHE_VERSION.trim(), def.ext);
        let content_type = def.content_type;
        let unavailable = unavailable
            .get(name)
            .map_or_else(|| "None".to_owned(), |reason| format!("Some({reason:?})"));
        // Filesystem-free wasm accessors need compile-time bytes. Native
        // accessors keep only store metadata and read the entry at run time.
        let (cfg, body) = if def.embed && embed_assets {
            (
                "",
                format!("    crate::asset::Asset::embedded(&{entry}, include_bytes!({path:?}))"),
            )
        } else {
            (
                "#[cfg(not(target_arch = \"wasm32\"))]\n",
                format!(
                    "    static BYTES: ::std::sync::OnceLock<Vec<u8>> = \
                     ::std::sync::OnceLock::new();\n    \
                     crate::asset::Asset::on_disk(&{entry}, &BYTES)"
                ),
            )
        };
        let _ = write!(
            out,
            "static {entry}: crate::asset::AssetEntry = crate::asset::AssetEntry {{\n    \
             name: {name:?},\n    id: {id:?},\n    path: {relative_path:?},\n    \
             content_type: {content_type:?},\n    unavailable: {unavailable},\n}};\n\n\
             {cfg}#[must_use]\n\
             pub fn {name}() -> crate::asset::Asset {{\n{body}\n}}\n\n",
        );
        let _ = writeln!(manifest, "    &{entry},");
        let _ = writeln!(
            by_name,
            "    {}({name:?}, {name}),",
            cfg.replace('\n', "\n        "),
        );
    }
    let _ = write!(
        out,
        "/// Every asset this build materialized.\n\
         pub static MANIFEST: &[&crate::asset::AssetEntry] = &[\n{manifest}];\n\n\
         type AssetLookup = (&'static str, fn() -> crate::asset::Asset);\n\n\
         static BY_NAME: &[AssetLookup] = &[\n{by_name}];\n\n\
         /// The asset this build registered under `name`.\n\
         #[must_use]\n\
         pub fn by_name(name: &str) -> Option<crate::asset::Asset> {{\n    \
         BY_NAME\n        .iter()\n        .find(|(candidate, _)| *candidate == name)\n        \
         .map(|(_, asset)| asset())\n}}\n",
    );
    out
}

fn main() {
    println!("cargo:rerun-if-env-changed={}", store::STORE_ENV);
    println!("cargo:rerun-if-env-changed={REMOTE_FIXTURES_ENV}");

    let defs: Vec<&AssetDef> = inventory::iter::<AssetDef>.into_iter().collect();
    assert!(
        !defs.is_empty(),
        "kithara-test-fixtures: the asset registry is empty; either `src/defs` declares no \
         `#[kithara::asset]`, or the registrations were dropped at link time",
    );
    for name in defs.iter().flat_map(|def| def.env) {
        println!("cargo:rerun-if-env-changed={name}");
    }
    let resolved = resolve(&defs);

    let fingerprint = store::CACHE_VERSION.trim();
    let root =
        store::root_from_env().unwrap_or_else(|error| panic!("kithara-test-fixtures: {error}"));
    let namespace = store::namespace(&root, fingerprint);
    let unavailable = materialize(&namespace, &resolved);
    for (_, id, def) in &resolved {
        println!(
            "cargo:rerun-if-changed={}",
            store::entry_path(&namespace, id, def.ext).display()
        );
    }

    // Written once per namespace so its mtime stays put on a no-op rerun. The
    // stamp tracks namespace removal; the declarations above track entries.
    let stamp = namespace.join(".stamp");
    if fs::read(&stamp).ok().as_deref() != Some(fingerprint.as_bytes()) {
        fs::write(&stamp, fingerprint.as_bytes())
            .unwrap_or_else(|error| panic!("kithara-test-fixtures: write stamp: {error}"));
    }
    println!("cargo:rerun-if-changed={}", stamp.display());

    let out_dir =
        PathBuf::from(std::env::var_os("OUT_DIR").expect("invariant: cargo always sets OUT_DIR"));
    let target_arch = std::env::var("CARGO_CFG_TARGET_ARCH")
        .unwrap_or_else(|_| panic!("invariant: cargo always sets CARGO_CFG_TARGET_ARCH"));
    fs::write(
        out_dir.join("assets.rs"),
        codegen(&namespace, &resolved, &unavailable, target_arch == "wasm32"),
    )
    .unwrap_or_else(|error| panic!("kithara-test-fixtures: write assets.rs: {error}"));
}
