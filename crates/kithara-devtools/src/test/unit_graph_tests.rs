use std::collections::BTreeSet;

use serde_json::{Value, json};

use super::unit_graph::UnitGraph;

fn unit(package: &str, dependencies: &[usize]) -> Value {
    json!({
        "pkg_id": package,
        "target": {"name": package, "kind": ["lib"], "crate_types": ["lib"], "src_path": format!("/{package}/lib.rs"), "edition": "2024"},
        "profile": {"name": "test"},
        "platform": null,
        "mode": "test",
        "features": [],
        "dependencies": dependencies.iter().map(|index| json!({
            "index": index, "extern_crate_name": "shared", "public": false,
        })).collect::<Vec<_>>(),
    })
}

fn parsed(value: &Value) -> UnitGraph {
    UnitGraph::parse(&serde_json::to_vec(value).unwrap()).unwrap()
}

fn graph() -> Value {
    json!({"version": 1, "units": [unit("required", &[1]), unit("shared", &[]), unit("optional", &[])], "roots": [0, 2]})
}

#[test]
fn selected_root_digests_preserve_dependency_edges_across_reindexing() {
    let full = parsed(&graph());
    let candidate = parsed(&json!({
        "version": 1, "units": [unit("shared", &[]), unit("required", &[0])], "roots": [1],
    }));
    let packages = BTreeSet::from(["required".to_owned()]);
    assert_eq!(
        full.closure_packages(&packages),
        BTreeSet::from(["required".to_owned(), "shared".to_owned()])
    );
    assert_eq!(
        full.selected_roots(&packages).unwrap(),
        candidate.all_roots().unwrap()
    );
}

#[test]
fn a_dependency_feature_profile_or_edge_change_invalidates_its_roots() {
    let packages = BTreeSet::from(["required".to_owned()]);
    let baseline = parsed(&graph()).selected_roots(&packages).unwrap();
    for changed in [
        ("features", json!(["extra"])),
        ("profile", json!({"name": "release"})),
        ("platform", json!("aarch64-apple-darwin")),
    ] {
        let mut value = graph();
        value["units"][1][changed.0] = changed.1;
        assert_ne!(baseline, parsed(&value).selected_roots(&packages).unwrap());
    }
    let mut value = graph();
    value["units"][0]["dependencies"][0]["extern_crate_name"] = json!("renamed");
    assert_ne!(baseline, parsed(&value).selected_roots(&packages).unwrap());
    let mut unrelated = graph();
    unrelated["units"][2]["features"] = json!(["extra"]);
    assert_eq!(
        baseline,
        parsed(&unrelated).selected_roots(&packages).unwrap()
    );
}

#[test]
fn malformed_or_unknown_graph_structure_is_rejected() {
    for (path, replacement) in [
        ("/version", json!(2)),
        ("/units/0/mode", json!("unknown")),
        ("/units/0/target/kind", json!(["unknown"])),
        ("/units/0/dependencies/0/index", json!(999)),
        ("/units/1/dependencies", json!([{"index": 0}])),
        ("/roots", json!([0, 0])),
        ("/roots", json!([999])),
    ] {
        let mut value = graph();
        *value.pointer_mut(path).unwrap() = replacement;
        assert!(
            UnitGraph::parse(&serde_json::to_vec(&value).unwrap()).is_err(),
            "{path}"
        );
    }
}

#[test]
fn an_empty_match_cannot_prove_parity_and_missing_active_roots_change_the_digest_set() {
    let full = parsed(&graph());
    assert!(
        full.selected_roots(&BTreeSet::from(["unknown".to_owned()]))
            .is_err()
    );
    let empty = parsed(&json!({"version": 1, "units": [], "roots": []}));
    assert!(empty.all_roots().is_err());
    let mut value = graph();
    value["units"][2]["pkg_id"] = json!("required");
    let expected = parsed(&value)
        .selected_roots(&BTreeSet::from(["required".to_owned()]))
        .unwrap();
    assert_ne!(
        expected,
        full.selected_roots(&BTreeSet::from(["required".to_owned()]))
            .unwrap()
    );
}

#[test]
fn an_inactive_package_does_not_invent_a_reference_root() {
    let full = parsed(&graph());
    let active = full
        .selected_roots(&BTreeSet::from(["required".to_owned()]))
        .unwrap();
    assert_eq!(
        active,
        full.selected_roots(&BTreeSet::from([
            "required".to_owned(),
            "inactive".to_owned()
        ]))
        .unwrap()
    );
}

#[test]
fn native_artifacts_must_cover_the_graph_and_keep_its_feature_sets() {
    let value = graph();
    let graph = parsed(&value);
    let messages = value["units"]
        .as_array()
        .unwrap()
        .iter()
        .map(|unit| {
            json!({
                "reason": "compiler-artifact", "package_id": unit["pkg_id"],
                "target": unit["target"], "profile": {"test": true}, "features": [],
            })
        })
        .collect::<Vec<_>>();
    let encode = |messages: &[Value]| {
        messages
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    };
    graph
        .verify_artifacts(encode(&messages).as_bytes())
        .unwrap();
    assert!(graph.verify_artifacts(b"").is_err());
    assert!(
        graph
            .verify_artifacts(encode(&messages[..1]).as_bytes())
            .is_err()
    );
    let mut changed = messages.clone();
    changed[1]["features"] = json!(["extra"]);
    assert!(graph.verify_artifacts(encode(&changed).as_bytes()).is_err());
}
