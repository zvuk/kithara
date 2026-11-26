use std::{collections::BTreeMap, io};

use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use super::*;

const PIN: &str = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";

fn fixture() -> Value {
    let id = "linux-lint@revision/linux/x86_64";
    let platform = json!({"os":"linux","arch":"x86_64"});
    let roles: BTreeMap<_, _> = ROLES
        .iter()
        .map(|role| {
            (
                *role,
                if *role == "gate" {
                    vec!["linux-lint"]
                } else {
                    vec![]
                },
            )
        })
        .collect();
    json!({"version":3,"schema_digest":SCHEMA_DIGEST,"consumer_source_pin":PIN,
        "operation":{"key":"op1","attempt":1,"predecessor_status_id":null,"original_kind":"branch",
            "event_kind":"push","logical_trust":"review","scope":"draft",
            "head_repository":{"id":2,"name":"source/project"},"status_repository":{"id":1,"name":"zvuk/kithara"},
            "execution":{"revision":{"repository":{"id":2,"name":"source/project"},"commit":"a".repeat(40),"tree":"d".repeat(40)}}},
        "request":{"identity":{"repository":"source/project","head":"a".repeat(40),"base":"b".repeat(40),
            "policy_revision":"c".repeat(40),"expected_trees":{"revision":"d".repeat(40)}},
            "kind":"branch","full":false,"only":["linux-lint"],"changed_paths":null,"narrow":null,"draft_tests":null,"parameters":{}},
        "attestation":{"catalog_digest":"e".repeat(64),"contract_digest":"f".repeat(64),"generation":"b".repeat(64),
            "raw_plan_sha256":"a".repeat(64),"dual_checks":[]},
        "checks":{(id):{"id":id,"definition":"linux-lint","purpose":"revision","platform":platform,
            "expected_tree":"d".repeat(40),"timeout_minutes":60,"needs":[],"source_contract_sha256":"a".repeat(64),
            "build_test_sha256":"b".repeat(64),"pins_sha256":"c".repeat(64)}},
        "graph":{"definitions":{"linux-lint":{"kind":"validation","platforms":["linux"],"kinds":["branch"],
            "capabilities":[],"needs":[],"parameters":[],"timeout_minutes":60,"path_globs":[],
            "recipe":{"family":"shared-lane","owner":"xtask/src/ci/lane/declared.rs","sources":{},
                "source_contract_sha256":"a".repeat(64),"role":"gate",
                "artifact":{"name":"hang","paths":[".ci-artifacts/hang"],"report":null,"when":"failure"},
                "semantics":{"kind":"validation","acceptance":"native-exit","capabilities":[],"reports":[],"automatic_kinds":null}}}},
            "roles":roles,"entrypoints":{".github/workflows/run.yml::run":["linux-lint"]},
            "non_validation":"comparison only; separate execution authority"},
        "harness_runners":{"tooling":{"nextest":{"filter":"package(xtask)"}}},
        "providers":{"github":[{"check":id,"pool":"pool-linux"}],"gitlab":[]}})
}

fn read(value: &Value) -> io::Result<PlanViewV3> {
    let bytes = canonical_json(value)?;
    decode(
        &bytes,
        &hex::encode(Sha256::digest(&bytes)),
        &ReadPins {
            schema_digest: SCHEMA_DIGEST,
            consumer_source_pin: PIN,
        },
    )
}

#[test]
fn canonical_wire_keeps_used_purpose_subset_and_independent_trust() {
    let mut value = fixture();
    value["operation"]["logical_trust"] = json!("quarantine");
    value["graph"]["non_validation"] = json!("unicode: é𝄞\u{7f}");
    let view = read(&value).unwrap();
    assert_eq!(view.operation.logical_trust, "quarantine");
    assert_eq!(view.operation.original_kind, "branch");
    assert_eq!(view.request.identity.expected_trees.len(), 1);
    assert_ne!(
        view.request.identity.policy_revision,
        view.request.identity.base
    );
    let bytes = canonical_json(&view).unwrap();
    assert!(bytes.is_ascii());
    assert!(
        String::from_utf8(bytes)
            .unwrap()
            .contains("\\u00e9\\ud834\\udd1e\\u007f")
    );
}

#[test]
fn closed_shapes_and_selected_graph_reject_inconsistent_data() {
    let edits: [fn(&mut Value); 9] = [
        |v| v["policy"] = json!({}),
        |v| v["graph"]["definitions"]["linux-lint"]["recipe"]["cache_group"] = json!("private"),
        |v| {
            v["graph"]["definitions"]["linux-lint"]["recipe"]
                .as_object_mut()
                .unwrap()
                .remove("role");
        },
        |v| v["operation"]["execution"]["revision"]["tree"] = json!("0".repeat(40)),
        |v| v["checks"]["linux-lint@revision/linux/x86_64"]["needs"] = json!(["missing"]),
        |v| v["providers"]["linux"] = json!([]),
        |v| {
            let row = v["providers"]["github"][0].clone();
            v["providers"]["github"].as_array_mut().unwrap().push(row);
        },
        |v| {
            v["graph"]["definitions"]["linux-lint"]["recipe"]["artifact"]["paths"] =
                json!(["../private"]);
        },
        |v| v["harness_runners"]["tooling"] = json!({"host":{"labels":["private"]}}),
    ];
    for (index, edit) in edits.iter().enumerate() {
        let mut value = fixture();
        edit(&mut value);
        assert!(read(&value).is_err(), "case {index}");
    }
}

#[test]
fn byte_reader_rejects_duplicate_keys_floats_noncanonical_and_wrong_pins() {
    let text = String::from_utf8(canonical_json(&fixture()).unwrap()).unwrap();
    for bytes in [
        text.replace("\"version\":3", "\"version\":3,\"version\":3")
            .into_bytes(),
        text.replace("\"version\":3", "\"version\":3.0")
            .into_bytes(),
        format!("{text}\n").into_bytes(),
    ] {
        assert!(
            decode(
                &bytes,
                &hex::encode(Sha256::digest(&bytes)),
                &ReadPins {
                    schema_digest: SCHEMA_DIGEST,
                    consumer_source_pin: PIN
                }
            )
            .is_err()
        );
    }
    assert!(
        decode(
            text.as_bytes(),
            &"0".repeat(64),
            &ReadPins {
                schema_digest: SCHEMA_DIGEST,
                consumer_source_pin: PIN
            }
        )
        .is_err()
    );
    assert!(
        decode(
            text.as_bytes(),
            &hex::encode(Sha256::digest(text.as_bytes())),
            &ReadPins {
                schema_digest: SCHEMA_DIGEST,
                consumer_source_pin: &"0".repeat(40)
            }
        )
        .is_err()
    );
}
