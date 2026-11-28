//! Path-gated jobs, evaluated the way GitLab evaluates `rules:changes`.

use std::collections::{BTreeMap, BTreeSet};

use serde_yaml_ng::Value;

use super::{GitlabConfig, workspace_root};

/// GitLab matches `changes:paths` with Ruby's `File.fnmatch?` under
/// `FNM_PATHNAME | FNM_DOTMATCH`: `*` and `?` stop at `/`, and only `**/`
/// spans directories. A trailing `/**` therefore reaches one level only.
fn gitlab_glob(pattern: &[u8], path: &[u8]) -> bool {
    if let Some(rest) = pattern.strip_prefix(b"**/") {
        return gitlab_glob(rest, path)
            || path
                .iter()
                .enumerate()
                .any(|(at, byte)| *byte == b'/' && gitlab_glob(rest, &path[at + 1..]));
    }
    match pattern.split_first() {
        None => path.is_empty(),
        Some((b'*', rest)) => (0..=path.len())
            .take_while(|end| path[..*end].iter().all(|byte| *byte != b'/'))
            .any(|end| gitlab_glob(rest, &path[end..])),
        Some((b'?', rest)) => {
            path.first().is_some_and(|byte| *byte != b'/') && gitlab_glob(rest, &path[1..])
        }
        Some((literal, rest)) => path.first() == Some(literal) && gitlab_glob(rest, &path[1..]),
    }
}

/// The job's rules with every `!reference [owner, rules]` spliced in place.
fn rules(config: &GitlabConfig, owner: &str) -> Vec<Value> {
    let mut flat = Vec::new();
    for rule in config.definition(owner)["rules"]
        .as_sequence()
        .unwrap_or_else(|| panic!("`{owner}` has a rule sequence"))
    {
        match rule {
            Value::Tagged(reference) => {
                let target = reference.value.as_sequence().expect("a rule reference");
                assert_eq!(target[1].as_str(), Some("rules"), "{owner}: {reference:?}");
                flat.extend(rules(config, target[0].as_str().expect("a rule owner")));
            }
            rule => flat.push(rule.clone()),
        }
    }
    flat
}

/// Evaluates the condition forms these rules use and refuses any other, so a
/// new form fails here instead of being read as true or false by accident.
fn holds(condition: &str, variables: &BTreeMap<&str, &str>) -> bool {
    if let Some((variable, _)) = condition.split_once(" && ") {
        // `$NAME && …` holds only while NAME is set, and no case here sets it.
        let variable = variable.trim().trim_start_matches('$');
        assert!(
            !variable.contains(' ') && !variables.contains_key(variable),
            "unsupported condition `{condition}`"
        );
        return false;
    }
    let (variable, value) = condition
        .split_once(" == ")
        .unwrap_or_else(|| panic!("unsupported condition `{condition}`"));
    let actual = variables
        .get(variable.trim().trim_start_matches('$'))
        .copied();
    match value.trim() {
        "null" => actual.is_none(),
        quoted => actual == Some(quoted.trim_matches('"')),
    }
}

/// Whether GitLab creates `job` in a pipeline started with `variables` whose
/// commit differs from `compare_to` in `changed`.
fn creates(
    config: &GitlabConfig,
    job: &str,
    variables: &BTreeMap<&str, &str>,
    changed: &[&str],
) -> bool {
    for rule in rules(config, job) {
        let rule = rule.as_mapping().expect("a rule is a mapping");
        if let Some(condition) = rule.get("if").and_then(Value::as_str)
            && !holds(condition, variables)
        {
            continue;
        }
        if let Some(changes) = rule.get("changes") {
            let base = changes["compare_to"]
                .as_str()
                .and_then(|base| base.strip_prefix('$'))
                .unwrap_or_else(|| panic!("`{job}` compares with a commit the run names"));
            assert!(
                variables.contains_key(base),
                "`{job}` compares with {base}, which this run does not set"
            );
            let reached = changes["paths"]
                .as_sequence()
                .expect("changed paths are a sequence")
                .iter()
                .filter_map(Value::as_str)
                .any(|glob| {
                    changed
                        .iter()
                        .any(|path| gitlab_glob(glob.as_bytes(), path.as_bytes()))
                });
            if !reached {
                continue;
            }
        }
        return rule.get("when").and_then(Value::as_str) != Some("never");
    }
    false
}

#[test]
fn the_glob_reaches_nested_files_only_through_a_double_star_directory() {
    let nested = b"tests/wasm-runner/src/driver.rs";
    assert!(!gitlab_glob(b"tests/wasm-runner/**", nested));
    assert!(gitlab_glob(b"tests/wasm-runner/**/*", nested));
    assert!(gitlab_glob(b"**/Cargo.toml", b"Cargo.toml"));
    assert!(gitlab_glob(b"**/Cargo.toml", b"crates/kithara/Cargo.toml"));
    assert!(gitlab_glob(b".cargo/config.toml", b".cargo/config.toml"));
}

/// A trailing `/**` is a gate GitLab never opens for a nested file, which is
/// how Safari stayed out of every review that changed its driver.
#[test]
fn no_path_gate_ends_in_a_double_star_that_stops_one_level_down() {
    let config = GitlabConfig::load(&workspace_root());
    for job in config
        .job_names()
        .filter(|job| !job.starts_with('.'))
        .filter(|job| !["default", "include", "stages", "variables", "workflow"].contains(job))
    {
        let Some(rules) = config.definition(job).get("rules") else {
            continue;
        };
        for rule in rules.as_sequence().into_iter().flatten() {
            let Some(paths) = rule.get("changes").and_then(|changes| changes.get("paths")) else {
                continue;
            };
            for glob in paths
                .as_sequence()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                assert!(
                    !glob.ends_with("/**"),
                    "`{job}` gates on `{glob}`, which GitLab matches one level deep only; \
                     write `{glob}/*`"
                );
            }
        }
    }
}

#[test]
fn a_review_runs_safari_exactly_when_it_changes_what_drives_the_browser() {
    let config = GitlabConfig::load(&workspace_root());
    let quarantine = BTreeMap::from([
        ("KITHARA_PIPELINE_KIND", "quarantine"),
        ("KITHARA_QUARANTINE_SHA", "head"),
        ("KITHARA_QUARANTINE_BASE_SHA", "base"),
    ]);
    for driver in [
        "tests/wasm-runner/src/driver.rs",
        "tests/wasm-runner/src/page/browser.js",
        "tests/wasm-runner-fixture/src/lib.rs",
        "xtask/src/wasm/command.rs",
        ".cargo/config.toml",
    ] {
        assert!(
            creates(&config, "apple:safari", &quarantine, &[driver]),
            "a review that changes {driver} runs Safari"
        );
    }
    assert!(
        !creates(
            &config,
            "apple:safari",
            &quarantine,
            &["crates/kithara-audio/src/lib.rs"]
        ),
        "a review that leaves the browser driver alone does not run Safari"
    );
}

fn main_merge() -> BTreeMap<&'static str, &'static str> {
    BTreeMap::from([
        ("KITHARA_PIPELINE_KIND", "main"),
        ("KITHARA_MAIN_BEFORE_SHA", "previous"),
    ])
}

/// The executable is replaced when a merge changes what it is built from, and
/// a merge that does not costs no Mac slot.
#[test]
fn a_merge_replaces_the_executable_only_when_it_changes_its_code() {
    let config = GitlabConfig::load(&workspace_root());
    for code in [
        "xtask/src/ci/verdict.rs",
        "crates/kithara-devtools/src/lib.rs",
        "Cargo.lock",
        ".cargo/config.toml",
    ] {
        assert!(
            creates(&config, "host:replace-binary", &main_merge(), &[code]),
            "a merge that changes {code} replaces the executable"
        );
    }
    assert!(
        !creates(
            &config,
            "host:replace-binary",
            &main_merge(),
            &["crates/kithara-audio/src/lib.rs", "docs/guides/ci-host.md"]
        ),
        "a merge that leaves the executable's code alone does not replace it"
    );
}

/// Every workspace crate the executable builds from, as Cargo resolves it,
/// reaches the gate, so a new dependency of xtask cannot leave a stale binary.
#[test]
fn the_replacement_gate_covers_every_crate_the_executable_builds_from() {
    let root = workspace_root();
    let metadata = cargo_metadata::MetadataCommand::new()
        .manifest_path(root.join("Cargo.toml"))
        .no_deps()
        .exec()
        .expect("cargo metadata");
    let packages: BTreeMap<&str, &cargo_metadata::Package> = metadata
        .packages
        .iter()
        .map(|package| (package.name.as_str(), package))
        .collect();
    let mut pending = vec!["xtask"];
    let mut seen = BTreeSet::new();
    while let Some(name) = pending.pop() {
        if !seen.insert(name) {
            continue;
        }
        for dependency in &packages[name].dependencies {
            if dependency.path.is_some()
                && dependency.kind == cargo_metadata::DependencyKind::Normal
            {
                pending.push(dependency.name.as_str());
            }
        }
    }
    let config = GitlabConfig::load(&root);
    for name in seen {
        let directory = packages[name]
            .manifest_path
            .parent()
            .expect("a manifest has a directory")
            .strip_prefix(&root)
            .expect("a workspace crate lives in the workspace");
        let source = format!("{directory}/src/lib.rs");
        assert!(
            creates(&config, "host:replace-binary", &main_merge(), &[&source]),
            "a change to {name} does not replace the executable built from it"
        );
    }
}
