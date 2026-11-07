//! Hosted GitHub gates that run before the full workspace tooling is available.

use std::{env, io};

use serde_json::Value;

/// Evaluate a hosted CI gate using the workflow's environment.
///
/// # Errors
/// Rejects missing or malformed inputs, unauthorized actors and unsuccessful required jobs.
pub fn run(mode: &str) -> io::Result<()> {
    match mode {
        "authorize" => authorize(&input("ACTOR")?, &input("OWNER")?, &input("RUNNER_LABELS")?),
        "required" => required(
            &input("RESULTS")?,
            &input("REQUIRED_LANES")?,
            &input("TOUCHED_UI_REQUIRED")?,
        ),
        _ => Err(io::Error::other(
            "expected GitHub gate authorize or required",
        )),
    }
}

fn input(name: &str) -> io::Result<String> {
    env::var(name).map_err(|error| io::Error::other(format!("{name}: {error}")))
}

fn authorize(actor: &str, owner: &str, labels: &str) -> io::Result<()> {
    if actor != owner || owner.is_empty() {
        return Err(io::Error::other(format!(
            "CI may only be started by repository owner {owner:?}, got {actor:?}"
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

fn required(results: &str, lanes: &str, touched_ui: &str) -> io::Result<()> {
    let results: serde_json::Map<String, Value> = serde_json::from_str(results)
        .map_err(|error| io::Error::other(format!("RESULTS: {error}")))?;
    let lanes: Vec<&str> = lanes.split_whitespace().collect();
    let ui = lanes.contains(&"all") || lanes.contains(&"deep-ui") || touched_ui == "true";
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
