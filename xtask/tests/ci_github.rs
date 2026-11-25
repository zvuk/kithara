use std::process::Command;

use serde_json::json;

fn authorize(event: Option<&str>, actor: &str, owner: &str, labels: &str) -> bool {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ci-bootstrap"));
    command
        .args(["github", "authorize"])
        .env_remove("EVENT_NAME")
        .env("ACTOR", actor)
        .env("OWNER", owner)
        .env("RUNNER_LABELS", labels);
    if let Some(event) = event {
        command.env("EVENT_NAME", event);
    }
    command
        .output()
        .expect("run the hosted authorization gate")
        .status
        .success()
}

#[test]
fn hosted_authorization_accepts_repository_pushes_from_maintainers() {
    for actor in ["owner", "maintainer"] {
        assert!(authorize(
            Some("push"),
            actor,
            "owner",
            r#"["self-hosted","linux"]"#,
        ));
    }
}

#[test]
fn hosted_authorization_keeps_manual_execution_owner_only() {
    for (actor, accepted) in [("owner", true), ("maintainer", false)] {
        assert_eq!(
            authorize(
                Some("workflow_dispatch"),
                actor,
                "owner",
                r#"["self-hosted"]"#,
            ),
            accepted,
        );
    }
}

#[test]
fn hosted_authorization_rejects_untrusted_unsupported_and_missing_events() {
    for actor in ["owner", "maintainer"] {
        for event in [
            None,
            Some(""),
            Some("pull_request"),
            Some("pull_request_target"),
            Some("repository_dispatch"),
            Some("workflow_call"),
            Some("schedule"),
            Some("unknown"),
        ] {
            assert!(
                !authorize(event, actor, "owner", r#"["self-hosted"]"#),
                "{event:?}/{actor}",
            );
        }
    }
}

#[test]
fn hosted_authorization_requires_actor_and_repository_owner() {
    for event in ["push", "workflow_dispatch"] {
        for (actor, owner) in [("", "owner"), ("owner", ""), ("", "")] {
            assert!(!authorize(Some(event), actor, owner, r#"["self-hosted"]"#));
        }
    }
}

#[test]
fn hosted_authorization_requires_nonempty_string_labels() {
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
        for (event, actor) in [("push", "maintainer"), ("workflow_dispatch", "owner")] {
            assert!(!authorize(Some(event), actor, "owner", labels), "{labels}");
        }
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
