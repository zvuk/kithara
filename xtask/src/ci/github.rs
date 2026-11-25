//! Hosted GitHub gates that run before the full workspace tooling is available.

use std::{env, fs::OpenOptions, io, io::Write};

use serde_json::Value;

/// Evaluate a hosted CI gate using the workflow's environment.
///
/// # Errors
/// Rejects missing or malformed inputs, unauthorized actors and unsuccessful required jobs.
pub fn run(mode: &str) -> io::Result<()> {
    match mode {
        "authorize" => authorize(
            &input("EVENT_NAME")?,
            &input("ACTOR")?,
            &input("OWNER")?,
            &input("RUNNER_LABELS")?,
        ),
        "kind" => publish_kind(
            &input("PULLS")?,
            &input("BASE_REPOSITORY")?,
            &input("GITHUB_OUTPUT")?,
        ),
        "required" => required(
            &input("RESULTS")?,
            &input("REQUIRED_LANES")?,
            &input("READY_UI_REQUIRED")?,
        ),
        _ => Err(io::Error::other(
            "expected GitHub gate authorize, kind or required",
        )),
    }
}

fn input(name: &str) -> io::Result<String> {
    env::var(name).map_err(|error| io::Error::other(format!("{name}: {error}")))
}

fn authorize(event: &str, actor: &str, owner: &str, labels: &str) -> io::Result<()> {
    let authorized = match event {
        "push" => true,
        "workflow_dispatch" => actor == owner,
        _ => false,
    };
    if !authorized || actor.is_empty() || owner.is_empty() {
        return Err(io::Error::other(format!(
            "CI requires a repository push or an owner workflow_dispatch, \
             got {event:?} by {actor:?} for {owner:?}"
        )));
    }
    let labels: Vec<String> = serde_json::from_str(labels)
        .map_err(|error| io::Error::other(format!("KITHARA_RUNNER_LABELS: {error}")))?;
    if labels.is_empty() || labels.iter().any(String::is_empty) {
        return Err(io::Error::other(
            "KITHARA_RUNNER_LABELS must be a non-empty JSON array of non-empty strings",
        ));
    }
    Ok(())
}

fn required(results: &str, lanes: &str, ready_ui: &str) -> io::Result<()> {
    let results: serde_json::Map<String, Value> = serde_json::from_str(results)
        .map_err(|error| io::Error::other(format!("RESULTS: {error}")))?;
    let lanes: Vec<&str> = lanes.split_whitespace().collect();
    let ui = lanes.contains(&"all") || lanes.contains(&"deep-ui") || ready_ui == "true";
    let android = lanes.contains(&"all") || lanes.contains(&"android-test");
    let mut incomplete = Vec::new();
    for (name, job) in results {
        let result = job["result"]
            .as_str()
            .ok_or_else(|| io::Error::other(format!("missing result for CI job {name}")))?;
        let optional = match name.as_str() {
            "deep" | "platforms" | "quality" => lanes.is_empty(),
            "ui" => !ui,
            "android" => !android,
            _ => false,
        };
        if result != "success" && !(result == "skipped" && optional) {
            incomplete.push(format!("{name}={result}"));
        }
    }
    if !incomplete.is_empty() {
        return Err(io::Error::other(format!(
            "required CI jobs did not execute successfully: {}",
            incomplete.join(", ")
        )));
    }
    Ok(())
}

/// The contents of the one fenced block a pull request description marks
/// `kithara-tests`, or none when it carries no such block.
///
/// # Errors
/// Rejects a second block and a block that is never closed: either way the
/// author asked for something this cannot read, and running without it would
/// hide that.
pub fn pr_tests_block(body: &str) -> io::Result<Option<String>> {
    let mut found = None;
    let mut lines = body.lines().map(|line| line.trim_end_matches('\r'));
    while let Some(line) = lines.next() {
        let Some(info) = line.trim_start().strip_prefix("```") else {
            continue;
        };
        let opened_here = info.trim() == "kithara-tests";
        let mut contents = Vec::new();
        let mut closed = false;
        for inner in lines.by_ref() {
            if inner.trim() == "```" {
                closed = true;
                break;
            }
            contents.push(inner);
        }
        if !opened_here {
            continue;
        }
        if !closed {
            return Err(io::Error::other(
                "the pull request's kithara-tests block is never closed",
            ));
        }
        if found.is_some() {
            return Err(io::Error::other(
                "the pull request carries a second kithara-tests block; keep one",
            ));
        }
        found = Some(contents.join("\n"));
    }
    Ok(found)
}

type PullSelection = (&'static str, Option<String>, Option<String>);

/// The pipeline kind of a push whose branch has the open pull requests
/// `pulls` (the GitHub pulls API's JSON array), the test block a draft runs,
/// and the pull request's base revision. A ready pull request is a
/// `merge-request`; a draft, or no pull request, is a `branch`.
fn pull_kind(pulls: &str) -> io::Result<PullSelection> {
    let pulls: Vec<Value> =
        serde_json::from_str(pulls).map_err(|error| io::Error::other(format!("PULLS: {error}")))?;
    if let Some(ready) = pulls
        .iter()
        .find(|pull| pull["draft"] == Value::Bool(false))
    {
        return Ok(("merge-request", None, Some(pull_base_sha(ready)?)));
    }
    let base = pulls.first().map(pull_base_sha).transpose()?;
    let mut block = None;
    for pull in &pulls {
        let body = pull["body"].as_str().unwrap_or_default();
        if let Some(found) = pr_tests_block(body)? {
            if block.is_some() {
                return Err(io::Error::other(
                    "more than one open pull request for this branch carries a kithara-tests block",
                ));
            }
            block = Some(found);
        }
    }
    Ok(("branch", block, base))
}

fn pull_base_sha(pull: &Value) -> io::Result<String> {
    pull.pointer("/base/sha")
        .and_then(Value::as_str)
        .filter(|sha| !sha.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| io::Error::other("open pull request has no base SHA"))
}

/// Writes the kind, the draft's test block and the comparison base as step outputs.
fn publish_kind(pulls: &str, upstream: &str, output: &str) -> io::Result<()> {
    let (kind, block, base) = pull_kind(pulls)?;
    let block = block.unwrap_or_default();
    let base_repository = base
        .as_ref()
        .map(|_| format!("base-repository={upstream}\n"))
        .unwrap_or_default();
    let base = base.unwrap_or_default();
    let delimiter = format!("KITHARA_PR_TESTS_{}", block.len());
    let mut file = OpenOptions::new().append(true).open(output)?;
    write!(
        file,
        "kind={kind}\nbase-sha={base}\n{base_repository}pr-tests<<{delimiter}\n{block}\n{delimiter}\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_description_without_a_block_names_nothing() {
        assert_eq!(
            pr_tests_block("Fixes seek.\n```rust\nlet x = 1;\n```").unwrap(),
            None
        );
    }

    #[test]
    fn the_block_contents_are_returned_without_the_fences() {
        let body =
            "Text\r\n```kithara-tests\r\npackages = [\"a\"]\r\nfilter = \"test(b)\"\r\n```\r\n";

        assert_eq!(
            pr_tests_block(body).unwrap().as_deref(),
            Some("packages = [\"a\"]\nfilter = \"test(b)\"")
        );
    }

    #[test]
    fn a_fence_inside_another_block_is_not_a_block() {
        let body = "```text\n```kithara-tests\n```\n";

        assert_eq!(pr_tests_block(body).unwrap(), None);
    }

    #[test]
    fn a_second_block_is_rejected() {
        let body = "```kithara-tests\n```\n```kithara-tests\n```\n";

        assert!(pr_tests_block(body).is_err());
    }

    #[test]
    fn an_unclosed_block_is_rejected() {
        assert!(pr_tests_block("```kithara-tests\npackages = []\n").is_err());
    }

    #[test]
    fn a_ready_pull_request_runs_full_and_ignores_the_block() {
        let pulls = r#"[{"draft": false, "body": "```kithara-tests\nfilter = 1\n```", "base": {"sha": "base"}}]"#;

        assert_eq!(
            pull_kind(pulls).unwrap(),
            ("merge-request", None, Some("base".to_owned()))
        );
    }

    #[test]
    fn a_draft_runs_its_block() {
        let pulls = r#"[{"draft": true, "body": "```kithara-tests\npackages = [\"a\"]\n```", "base": {"sha": "base"}}]"#;

        assert_eq!(
            pull_kind(pulls).unwrap(),
            (
                "branch",
                Some("packages = [\"a\"]".to_owned()),
                Some("base".to_owned())
            )
        );
    }

    #[test]
    fn a_branch_without_a_pull_request_is_a_draft_with_no_block() {
        assert_eq!(pull_kind("[]").unwrap(), ("branch", None, None));
    }

    #[test]
    fn a_draft_without_a_description_has_no_block() {
        assert_eq!(
            pull_kind(r#"[{"draft": true, "body": null, "base": {"sha": "base"}}]"#).unwrap(),
            ("branch", None, Some("base".to_owned()))
        );
    }

    #[test]
    fn base_repository_is_published_only_with_a_pull_request_base() {
        for (pulls, upstream_is_published) in [
            ("[]", false),
            (
                r#"[{"draft": false, "body": null, "base": {"sha": "base"}}]"#,
                true,
            ),
        ] {
            let output = tempfile::NamedTempFile::new().expect("temporary GitHub output");
            publish_kind(
                pulls,
                "zvuk/kithara",
                output.path().to_str().expect("UTF-8 temporary path"),
            )
            .expect("kind outputs are written");
            let text = std::fs::read_to_string(output.path()).expect("kind outputs are readable");
            assert_eq!(
                text.contains("base-repository=zvuk/kithara"),
                upstream_is_published,
                "{text}"
            );
        }
    }
}
