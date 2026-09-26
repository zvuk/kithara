use kithara_test_utils::kithara;
use kithara_ui::{
    envelope::{DocKind, probe},
    error::UiDocError,
    ids::{DocId, SourceUri},
};

fn origin() -> SourceUri {
    SourceUri("test.ron".into())
}

#[kithara::test]
fn probes_layout_envelope_ignoring_body() {
    let env = probe(
        r#"(schema: "kithara.layout", version: 1, id: "micro", root: ())"#,
        &origin(),
    )
    .unwrap();
    assert_eq!(env.kind, DocKind::Layout);
    assert_eq!(env.version, 1);
    assert_eq!(env.id, DocId("micro".into()));
}

#[kithara::test]
fn rejects_unknown_schema() {
    let err = probe(
        r#"(schema: "kithara.nope", version: 1, id: "x")"#,
        &origin(),
    )
    .unwrap_err();
    assert!(matches!(
        err,
        UiDocError::UnknownSchema { schema, .. } if schema == "kithara.nope"
    ));
}

#[kithara::test]
fn rejects_future_version() {
    let err = probe(
        r#"(schema: "kithara.module", version: 99, id: "x")"#,
        &origin(),
    )
    .unwrap_err();
    assert!(matches!(
        err,
        UiDocError::UnsupportedVersion {
            version: 99,
            max: 1,
            ..
        }
    ));
}

#[kithara::test]
fn syntax_error_carries_origin() {
    let err = probe("(((", &origin()).unwrap_err();
    assert!(err.to_string().starts_with("test.ron:"));
}
