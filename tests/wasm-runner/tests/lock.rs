//! The bindings the runner generates must match what the test binaries link.
//!
//! `wasm-bindgen` refuses glue whose schema differs from the runtime's, and
//! `wasm-bindgen-test` pins its harness to one bindgen release. One version of
//! each, all equal, keeps the generator, the runtime and the harness in step.

use std::{collections::BTreeMap, fs, path::Path};

use kithara_test_utils::kithara;

const PACKAGES: [&str; 3] = [
    "wasm-bindgen",
    "wasm-bindgen-cli-support",
    "wasm-bindgen-test-shared",
];

#[kithara::test(native)]
fn generator_runtime_and_harness_resolve_to_one_bindgen_version() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.lock");
    let text = fs::read_to_string(&path).expect("the workspace lock file is readable");
    let lock: toml::Table = toml::from_str(&text).expect("the lock file parses");
    let mut versions: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    let packages = lock["package"].as_array().expect("the lock lists packages");
    for package in packages {
        let name = package["name"].as_str().unwrap_or_default();
        if PACKAGES.contains(&name) || name == "wasm-bindgen-test" {
            let version = package["version"].as_str().unwrap_or_default();
            versions.entry(name).or_default().push(version);
        }
    }
    assert_eq!(
        versions.get("wasm-bindgen-test").map(Vec::len),
        Some(1),
        "one wasm-bindgen-test harness: {versions:?}"
    );
    let resolved: Vec<_> = PACKAGES.iter().map(|name| versions.get(name)).collect();
    assert!(
        resolved
            .iter()
            .all(|found| found.is_some_and(|list| list.len() == 1)),
        "each of {PACKAGES:?} resolves to exactly one version: {versions:?}"
    );
    assert!(
        resolved.windows(2).all(|pair| pair[0] == pair[1]),
        "{PACKAGES:?} share one version: {versions:?}"
    );
}
