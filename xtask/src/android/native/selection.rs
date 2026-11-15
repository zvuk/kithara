//! Nextest's resolved case selection for native device batches.

use std::{collections::BTreeMap, path::PathBuf};

use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;

/// Exact libtest name filters; an empty filter runs a fully selected binary.
pub(super) fn filters(bytes: &[u8]) -> Result<BTreeMap<PathBuf, Vec<String>>> {
    let inventory: Value = serde_json::from_slice(bytes)?;
    let suites = inventory["rust-suites"]
        .as_object()
        .context("Android selection has no Rust suites")?;
    let mut plan = BTreeMap::new();
    for (id, suite) in suites {
        ensure!(
            !id.is_empty() && suite["binary-id"].as_str() == Some(id),
            "Android selection has an inconsistent binary identity"
        );
        match suite["status"].as_str() {
            Some("listed") => {}
            Some("skipped" | "skipped-default-filter") => continue,
            status => bail!("Android selection has unknown suite status {status:?}"),
        }
        let path = suite["binary-path"]
            .as_str()
            .filter(|path| !path.is_empty())
            .context("Android selection has no binary path")?;
        let cases = suite["testcases"]
            .as_object()
            .context("Android selection has no test cases")?;
        let mut runnable = 0;
        let mut selected = Vec::new();
        for (name, case) in cases {
            ensure!(!name.is_empty(), "Android selection has an empty test name");
            let ignored = case["ignored"]
                .as_bool()
                .context("Android selection has no ignored flag")?;
            runnable += usize::from(!ignored);
            match case["filter-match"]["status"].as_str() {
                Some("matches") if !ignored => selected.push(name.clone()),
                Some("matches" | "mismatch") => {}
                status => bail!("Android selection has unknown case status {status:?}"),
            }
        }
        if selected.is_empty() {
            continue;
        }
        if selected.len() == runnable {
            selected.clear();
        }
        selected.sort();
        ensure!(
            plan.insert(PathBuf::from(path), selected).is_none(),
            "Android selection repeats binary {path}"
        );
    }
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn inventory() -> Value {
        json!({"rust-suites": {"demo::tests": {
            "binary-id": "demo::tests", "binary-path": "/cargo/deps/demo",
            "status": "listed", "testcases": {
                "selected": {"ignored": false, "filter-match": {"status": "matches"}},
                "excluded": {"ignored": false, "filter-match": {"status": "mismatch"}},
                "ignored": {"ignored": true, "filter-match": {"status": "matches"}}
            }
        }}})
    }

    #[test]
    fn a_filtered_binary_names_only_its_runnable_selected_cases() {
        let mut listing = inventory();
        listing["rust-suites"]["skipped::tests"] = json!({
            "binary-id": "skipped::tests", "status": "skipped-default-filter"
        });
        assert_eq!(
            filters(&serde_json::to_vec(&listing).unwrap()).unwrap(),
            BTreeMap::from([(PathBuf::from("/cargo/deps/demo"), vec!["selected".into()])])
        );
    }

    #[test]
    fn a_fully_selected_binary_keeps_one_unfiltered_batch() {
        let mut listing = inventory();
        listing["rust-suites"]["demo::tests"]["testcases"]["excluded"]["filter-match"]["status"] =
            json!("matches");
        assert_eq!(
            filters(&serde_json::to_vec(&listing).unwrap()).unwrap(),
            BTreeMap::from([(PathBuf::from("/cargo/deps/demo"), Vec::new())])
        );
    }

    #[test]
    fn a_binary_with_no_selected_case_is_not_invoked() {
        let mut listing = inventory();
        listing["rust-suites"]["demo::tests"]["testcases"]["selected"]["filter-match"]["status"] =
            json!("mismatch");
        assert!(
            filters(&serde_json::to_vec(&listing).unwrap())
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn unknown_selection_states_and_duplicate_images_are_refused() {
        let mut suite = inventory();
        suite["rust-suites"]["demo::tests"]["status"] = json!("unknown");
        let mut case = inventory();
        case["rust-suites"]["demo::tests"]["testcases"]["selected"]["filter-match"]["status"] =
            json!("unknown");
        let mut duplicate = inventory();
        duplicate["rust-suites"]["other::tests"] = duplicate["rust-suites"]["demo::tests"].clone();
        duplicate["rust-suites"]["other::tests"]["binary-id"] = json!("other::tests");
        for listing in [json!({}), suite, case, duplicate] {
            assert!(filters(&serde_json::to_vec(&listing).unwrap()).is_err());
        }
    }
}
