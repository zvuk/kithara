use std::collections::BTreeSet;

use serde_yaml_ng::Value;

use super::{
    GitlabConfig, POOL_TAG, declared_kinds, lane_catalog, mapping, rule_kinds, workspace_root, yaml,
};

const REVIEW_KINDS: [&str; 3] = ["branch", "merge-request", "quarantine"];
const MAC_TAG_PREFIX: &str = "kithara-macos";
const DRAFT_TEST_CHECK: &str = "apple:pr-tests";
const REUSE_RULES: &str = ".reused-from-draft";
const REPEAT_RULES: &str = ".repeated-jobs";

/// Every job a pipeline file defines: a visible name with a script.
fn jobs(config: &GitlabConfig) -> Vec<&str> {
    config
        .documents
        .iter()
        .flat_map(|document| mapping(document, "a GitLab pipeline file"))
        .filter_map(|(name, job)| {
            let name = name.as_str()?;
            let runs = job.as_mapping()?.contains_key("script");
            (runs && !name.starts_with('.')).then_some(name)
        })
        .collect()
}

fn tags(config: &GitlabConfig, job: &str) -> Vec<String> {
    config
        .effective_value(job, "tags")
        .and_then(|tags| tags.as_sequence().cloned())
        .unwrap_or_default()
        .iter()
        .map(|tag| tag.as_str().expect("a runner tag").to_owned())
        .collect()
}

fn on_mac(config: &GitlabConfig, job: &str) -> bool {
    tags(config, job)
        .iter()
        .any(|tag| tag.starts_with(MAC_TAG_PREFIX))
}

fn in_review(config: &GitlabConfig, job: &str) -> bool {
    let kinds = config.admitted_kinds(job);
    REVIEW_KINDS.iter().any(|kind| kinds.contains(*kind))
}

fn automatic(config: &GitlabConfig, job: &str) -> bool {
    config
        .effective_value(job, "when")
        .as_ref()
        .and_then(Value::as_str)
        != Some("manual")
}

fn needs(config: &GitlabConfig, job: &str) -> Vec<Value> {
    config
        .effective_value(job, "needs")
        .and_then(|needs| needs.as_sequence().cloned())
        .unwrap_or_default()
}

fn needed_job(need: &Value) -> &str {
    need.as_str()
        .or_else(|| need.get("job").and_then(Value::as_str))
        .expect("a need names a job")
}

/// Whether a job's rules, or any rules they reference, carry `owner`'s.
fn references_rules(config: &GitlabConfig, rules_owner: &str, owner: &str) -> bool {
    config.definition(rules_owner)["rules"]
        .as_sequence()
        .expect("rules are a sequence")
        .iter()
        .any(|rule| match rule {
            Value::Tagged(reference) => {
                let target = reference.value[0]
                    .as_str()
                    .expect("GitLab reference owner is a string");
                target == owner || references_rules(config, target, owner)
            }
            _ => false,
        })
}

#[test]
fn review_mac_jobs_wait_only_for_data() {
    let config = GitlabConfig::load(&workspace_root());
    let draft_check: Value =
        serde_yaml_ng::from_str(&format!("{{job: {DRAFT_TEST_CHECK}, optional: true}}")).unwrap();
    let mut checked = 0;
    for job in jobs(&config) {
        if !on_mac(&config, job) || !in_review(&config, job) || !automatic(&config, job) {
            continue;
        }
        checked += 1;
        for need in needs(&config, job) {
            let downloads = need.get("artifacts").and_then(Value::as_bool) == Some(true);
            assert!(
                downloads || need == draft_check,
                "`{job}` waits for `{}` without downloading its artifacts",
                needed_job(&need)
            );
        }
    }
    assert!(checked > 0, "no review Mac job was checked");
}

#[test]
fn no_linux_job_waits_for_a_mac_job() {
    let config = GitlabConfig::load(&workspace_root());
    for job in jobs(&config) {
        if !tags(&config, job).iter().any(|tag| tag == POOL_TAG) {
            continue;
        }
        for need in needs(&config, job) {
            let needed = needed_job(&need);
            assert!(
                !on_mac(&config, needed),
                "Linux job `{job}` waits for Mac job `{needed}`"
            );
        }
    }
}

#[test]
fn review_mac_jobs_are_defined_longest_chain_first() {
    let order = [
        DRAFT_TEST_CHECK,
        "apple:net-units",
        "apple:xcframework",
        "apple:ios-test",
        "apple:ios",
        "apple:test",
        "apple:lint",
    ];
    let apple = yaml(workspace_root().join(".gitlab/ci/apple.yml"));
    let defined: Vec<&str> = mapping(&apple, "the Apple pipeline file")
        .keys()
        .filter_map(Value::as_str)
        .filter(|name| order.contains(name))
        .collect();
    assert_eq!(defined, order);
}

/// Every job that downloads another's artifacts in a run of `kind`, by the
/// job it downloads them from.
fn artifact_producers(config: &GitlabConfig, kind: &str) -> BTreeSet<String> {
    jobs(config)
        .into_iter()
        .filter(|job| config.admitted_kinds_alone(job).contains(kind))
        .flat_map(|job| needs(config, job))
        .filter(|need| {
            need.get("artifacts").and_then(Value::as_bool) == Some(true)
                && need.get("optional").and_then(Value::as_bool) != Some(true)
        })
        .map(|need| needed_job(&need).to_owned())
        .collect()
}

/// Whether `owner`'s rules, or any they reference, leave a run of `kind` to
/// start the job by hand.
fn manual_in(config: &GitlabConfig, owner: &str, kind: &str) -> bool {
    config.definition(owner)["rules"]
        .as_sequence()
        .expect("rules are a sequence")
        .iter()
        .any(|rule| match rule {
            Value::Tagged(reference) => manual_in(
                config,
                reference.value[0]
                    .as_str()
                    .expect("GitLab reference owner is a string"),
                kind,
            ),
            Value::Mapping(rule) => {
                rule.get("when").and_then(Value::as_str) == Some("manual")
                    && rule_kinds(
                        rule.get("if").and_then(Value::as_str).unwrap_or_default(),
                        true,
                    )
                    .contains(&kind)
            }
            _ => false,
        })
}

/// Whether `owner`'s rules reach `target`'s before any rule that admits a run
/// of `kind`: a refusal placed after an admission never fires.
fn refuses_first(config: &GitlabConfig, owner: &str, target: &str, kind: &str) -> Option<bool> {
    for rule in config.definition(owner)["rules"]
        .as_sequence()
        .expect("rules are a sequence")
    {
        match rule {
            Value::Tagged(reference) => {
                let referenced = reference.value[0]
                    .as_str()
                    .expect("GitLab reference owner is a string");
                if referenced == target {
                    return Some(true);
                }
                if let Some(first) = refuses_first(config, referenced, target, kind) {
                    return Some(first);
                }
            }
            Value::Mapping(rule) => {
                let admits = rule.get("when").and_then(Value::as_str) != Some("never")
                    && rule_kinds(
                        rule.get("if").and_then(Value::as_str).unwrap_or_default(),
                        true,
                    )
                    .contains(&kind);
                if admits {
                    return Some(false);
                }
            }
            _ => panic!("`{owner}` has an invalid rule"),
        }
    }
    None
}

/// A ready run skips exactly the jobs its draft already ran on the same merge
/// tree as the very same plan: admitted in both kinds, the same arguments, no
/// suite the draft scope narrows, and no artifacts another job reads.
#[test]
fn a_ready_run_skips_exactly_the_jobs_its_draft_answered_for() {
    let root = workspace_root();
    let config = GitlabConfig::load(&root);
    let catalog = lane_catalog(&root);
    let producers = artifact_producers(&config, "quarantine");

    let reused: BTreeSet<String> = jobs(&config)
        .into_iter()
        .filter(|job| {
            config
                .rules_owner(job)
                .is_some_and(|owner| references_rules(&config, &owner, REUSE_RULES))
        })
        .map(str::to_owned)
        .collect();
    let mut answered = BTreeSet::new();
    for (job, lane) in config.lane_jobs() {
        let Some(entry) = catalog.get(lane.as_str()) else {
            continue;
        };
        let admitted = config.admitted_kinds(&job);
        let placed = declared_kinds(entry);
        let in_both = ["branch", "quarantine"]
            .iter()
            .all(|kind| admitted.contains(*kind) && placed.contains(*kind));
        let owner = config.rules_owner(&job).expect("an admitted job has rules");
        let manual = ["branch", "quarantine"]
            .iter()
            .any(|kind| manual_in(&config, &owner, kind));
        if !in_both || manual || producers.contains(&job) {
            continue;
        }
        let steps = entry["steps"].as_array().expect("lane steps are an array");
        let same_plan = steps.iter().all(|step| {
            let args = |kind: &str| {
                step.get("args_by_kind")
                    .and_then(|by_kind| by_kind.get(kind))
                    .unwrap_or_else(|| &step["args"])
                    .clone()
            };
            let suite = args("quarantine")
                .as_array()
                .and_then(|args| args.first().cloned())
                .is_some_and(|first| first.as_str() == Some("test"));
            args("branch") == args("quarantine") && !suite
        });
        if same_plan {
            answered.insert(job);
        }
    }

    assert!(!answered.is_empty(), "no job is answered for by its draft");
    assert_eq!(reused, answered);
}

/// A repeat of a rejected run creates the jobs it names and nothing else, so
/// every job a ready run admits refuses an unnamed repeat before admitting -
/// except a job whose artifacts another reads, which the reader cannot do
/// without.
#[test]
fn a_repeat_creates_only_the_jobs_it_names_and_what_they_read() {
    let config = GitlabConfig::load(&workspace_root());
    let producers = artifact_producers(&config, "quarantine");
    let mut checked = 0;
    for job in jobs(&config) {
        if !config.admitted_kinds_alone(job).contains("quarantine") {
            continue;
        }
        checked += 1;
        let owner = config
            .rules_owner(job)
            .unwrap_or_else(|| panic!("`{job}` has no rules"));
        assert_eq!(
            refuses_first(&config, &owner, REPEAT_RULES, "quarantine") == Some(true),
            !producers.contains(job),
            "`{job}`"
        );
    }
    assert!(checked > 0, "no ready-run job was checked");
}
