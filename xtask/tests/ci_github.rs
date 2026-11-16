use std::process::Command;

use serde_json::json;

fn authorize(actor: &str, labels: &str) -> bool {
    Command::new(env!("CARGO_BIN_EXE_ci-bootstrap"))
        .args(["github", "authorize"])
        .env("ACTOR", actor)
        .env("OWNER", "owner")
        .env("RUNNER_LABELS", labels)
        .output()
        .expect("run the hosted authorization gate")
        .status
        .success()
}

#[test]
fn hosted_authorization_requires_the_owner_and_nonempty_string_labels() {
    assert!(authorize("owner", r#"["self-hosted","linux"]"#));
    assert!(!authorize("other", r#"["self-hosted"]"#));
    for labels in [
        "",
        "{",
        "null",
        "{}",
        "[]",
        r#"[""]"#,
        "[42]",
        r#"["ok",false]"#,
    ] {
        assert!(!authorize("owner", labels), "{labels}");
    }
}

#[test]
fn hosted_verdict_never_accepts_failed_cancelled_or_required_skipped_jobs() {
    for (lanes, name, result, accepted) in [
        ("", "lanes", "success", true),
        ("", "lanes", "skipped", false),
        ("", "lanes", "failure", false),
        ("", "lanes", "cancelled", false),
        ("", "deep", "skipped", true),
        ("all", "deep", "skipped", false),
        ("", "deep", "failure", false),
        ("", "android", "skipped", true),
        ("android-test", "android", "skipped", false),
        ("all", "android", "skipped", false),
        ("", "unknown", "skipped", false),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_ci-bootstrap"))
            .args(["github", "required"])
            .env("RESULTS", json!({name: {"result": result}}).to_string())
            .env("REQUIRED_LANES", lanes)
            .env("READY_UI_REQUIRED", "false")
            .output()
            .unwrap();
        assert_eq!(
            output.status.success(),
            accepted,
            "{lanes}/{name}/{result}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
