//! Every maintained AST rule belongs to the repository rule library.

use std::{collections::BTreeMap, fs, path::Path};

const CONSUMER_RULES: [&str; 3] = [
    "perf.no-collect-iter-roundtrip",
    "rust.no-debug-prints",
    "rust.no-inherent-to-string",
];

fn rule_sources(directory: &Path) -> BTreeMap<String, String> {
    let mut rules = BTreeMap::new();
    for entry in fs::read_dir(directory).expect("read rule directory") {
        let path = entry.expect("read rule entry").path();
        if !matches!(
            path.extension().and_then(|extension| extension.to_str()),
            Some("yml" | "yaml")
        ) {
            continue;
        }
        let text = fs::read_to_string(&path).expect("read rule source");
        let ids: Vec<_> = text
            .lines()
            .filter_map(|line| line.strip_prefix("id:"))
            .map(str::trim)
            .collect();
        assert!(!ids.is_empty(), "{} declares no rule ID", path.display());
        for id in ids {
            assert!(
                rules
                    .insert(id.to_string(), path.display().to_string())
                    .is_none(),
                "duplicate maintained rule: {id}"
            );
        }
    }
    rules
}

#[test]
fn every_rule_has_one_canonical_repository_owner() {
    let package = Path::new(env!("CARGO_MANIFEST_DIR"));
    assert!(
        !package.join("rules").exists(),
        "devtools must not own a second rule library"
    );
    let rules = rule_sources(&package.join("../../.config/ast-grep"));
    assert!(
        !rules.is_empty(),
        "canonical rule library must not be empty"
    );
    for id in CONSUMER_RULES {
        assert!(rules.contains_key(id), "missing canonical rule {id}");
    }
}
