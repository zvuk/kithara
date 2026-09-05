use std::{collections::BTreeSet, fs, path::Path};

use serde_yaml_ng::{Mapping, Value};

const MERGE_REQUEST_KIND: &str = "merge-request";
const VERDICT_REPORT_DIR: &str = ".ci-artifacts/junit/";

struct GitlabConfig {
    documents: Vec<Value>,
}

impl GitlabConfig {
    fn load(root: &Path) -> Self {
        let mut paths: Vec<_> = fs::read_dir(root.join(".gitlab/ci"))
            .expect("the pipeline directory is readable")
            .map(|entry| entry.expect("a pipeline entry is readable").path())
            .collect();
        paths.sort();
        Self {
            documents: paths.into_iter().map(yaml).collect(),
        }
    }

    fn definition(&self, name: &str) -> &Mapping {
        self.documents
            .iter()
            .find_map(|document| document.as_mapping()?.get(name)?.as_mapping())
            .unwrap_or_else(|| panic!("GitLab configuration has no `{name}` definition"))
    }

    /// Every top-level name the pipeline files declare. Job names are unique
    /// across the directory, so which file a name came from says nothing.
    fn job_names(&self) -> impl Iterator<Item = &str> {
        self.documents
            .iter()
            .flat_map(|document| mapping(document, "a GitLab pipeline file").keys())
            .filter_map(Value::as_str)
    }

    /// Every job that runs a declared lane, as (job name, lane name). The
    /// Windows host carries no `just`, so it reaches xtask through cargo;
    /// both spellings name the same lane, and the lane name is the first
    /// word after the subcommand.
    fn lane_jobs(&self) -> Vec<(String, String)> {
        const INVOCATIONS: [&str; 2] = ["just ci run ", "cargo run --locked -p xtask -- ci run "];

        let mut jobs = Vec::new();
        for document in &self.documents {
            for (name, job) in mapping(document, "a GitLab pipeline file") {
                let (Some(name), Some(job)) = (name.as_str(), job.as_mapping()) else {
                    continue;
                };
                let Some(script) = job.get("script").and_then(Value::as_sequence) else {
                    continue;
                };
                for line in script.iter().filter_map(Value::as_str) {
                    let line = line.trim();
                    let Some(lane) = INVOCATIONS
                        .iter()
                        .find_map(|invocation| line.strip_prefix(invocation))
                    else {
                        continue;
                    };
                    let lane = lane
                        .split_whitespace()
                        .next()
                        .unwrap_or_else(|| panic!("`{name}` runs a lane with no name"));
                    jobs.push((name.to_owned(), lane.to_owned()));
                }
            }
        }
        jobs
    }

    /// The pipeline kinds a job's rules admit, following `extends` and
    /// `!reference` to wherever the rules actually live.
    fn admitted_kinds(&self, job: &str) -> BTreeSet<String> {
        match self.rules_owner(job) {
            None => BTreeSet::new(),
            Some(owner) => self.admitted_kinds_inner(&owner, &mut Vec::new()),
        }
    }

    fn admitted_kinds_inner(&self, owner: &str, stack: &mut Vec<String>) -> BTreeSet<String> {
        assert!(
            !stack.iter().any(|seen| seen == owner),
            "GitLab rule cycle through `{owner}`"
        );
        stack.push(owner.to_owned());

        let mut kinds = BTreeSet::new();
        let rules = self
            .definition(owner)
            .get("rules")
            .unwrap_or_else(|| panic!("`{owner}` owns no rules"))
            .as_sequence()
            .unwrap_or_else(|| panic!("`{owner}` rules are not a sequence"));
        for rule in rules {
            match rule {
                // A rule that guards on anything other than the pipeline kind -
                // a platform filter, the release-lane switch - admits no kind of
                // its own. It only ever refuses one the referenced sets admit.
                Value::Mapping(rule) => {
                    let Some(kind) = rule
                        .get("if")
                        .and_then(Value::as_str)
                        .and_then(declared_kind)
                    else {
                        continue;
                    };
                    if rule.get("when").and_then(Value::as_str) == Some("never") {
                        continue;
                    }
                    kinds.insert(kind.to_owned());
                }
                Value::Tagged(reference) => {
                    assert!(reference.tag == "reference", "unknown GitLab YAML tag");
                    let target = reference
                        .value
                        .as_sequence()
                        .expect("GitLab reference target is a sequence");
                    assert_eq!(target.len(), 2, "GitLab reference has two components");
                    assert_eq!(target[1].as_str(), Some("rules"));
                    let target = target[0]
                        .as_str()
                        .expect("GitLab reference owner is a string");
                    kinds.extend(self.admitted_kinds_inner(target, stack));
                }
                _ => panic!("`{owner}` has an invalid rule"),
            }
        }

        stack.pop();
        kinds
    }

    fn extends(&self, name: &str) -> Vec<&str> {
        match self.definition(name).get("extends") {
            None => Vec::new(),
            Some(Value::String(parent)) => vec![parent],
            Some(Value::Sequence(parents)) => parents
                .iter()
                .map(|parent| {
                    parent
                        .as_str()
                        .unwrap_or_else(|| panic!("`{name}` has a non-string parent"))
                })
                .collect(),
            Some(_) => panic!("`{name}` has invalid parents"),
        }
    }

    fn rules_owner(&self, name: &str) -> Option<String> {
        self.rules_owner_inner(name, &mut Vec::new())
    }

    fn rules_owner_inner(&self, name: &str, stack: &mut Vec<String>) -> Option<String> {
        assert!(
            !stack.iter().any(|parent| parent == name),
            "GitLab inheritance cycle through `{name}`"
        );
        stack.push(name.to_owned());

        let mut owner = None;
        for parent in self.extends(name) {
            if let Some(parent_owner) = self.rules_owner_inner(parent, stack) {
                owner = Some(parent_owner);
            }
        }
        if self.definition(name).contains_key("rules") {
            owner = Some(name.to_owned());
        }

        stack.pop();
        owner
    }

    fn effective_value(&self, name: &str, key: &str) -> Option<Value> {
        self.effective_value_inner(name, key, &mut Vec::new())
    }

    fn effective_value_inner(
        &self,
        name: &str,
        key: &str,
        stack: &mut Vec<String>,
    ) -> Option<Value> {
        assert!(
            !stack.iter().any(|parent| parent == name),
            "GitLab inheritance cycle through `{name}`"
        );
        stack.push(name.to_owned());

        let mut value = None;
        for parent in self.extends(name) {
            if let Some(parent_value) = self.effective_value_inner(parent, key, stack) {
                value = Some(parent_value);
            }
        }
        if let Some(own_value) = self.definition(name).get(key) {
            value = Some(own_value.clone());
        }

        stack.pop();
        value
    }

    fn decision_for_kind(&self, rules_owner: &str, kind: &str) -> Option<RuleDecision> {
        let rules = self
            .definition(rules_owner)
            .get("rules")
            .unwrap_or_else(|| panic!("`{rules_owner}` owns no rules"))
            .as_sequence()
            .unwrap_or_else(|| panic!("`{rules_owner}` rules are not a sequence"));

        for rule in rules {
            match rule {
                Value::Mapping(rule) => {
                    for key in rule.keys() {
                        let key = key
                            .as_str()
                            .unwrap_or_else(|| panic!("`{rules_owner}` has a non-string rule key"));
                        assert!(
                            matches!(key, "if" | "when"),
                            "`{rules_owner}` uses unsupported rule key `{key}`"
                        );
                    }
                    let condition = rule
                        .get("if")
                        .unwrap_or_else(|| panic!("`{rules_owner}` has an unconditional rule"));
                    let matches = pipeline_kind(condition, rules_owner) == kind;
                    if matches {
                        return Some(RuleDecision {
                            when: rule.get("when").map(|when| {
                                when.as_str()
                                    .unwrap_or_else(|| {
                                        panic!("`{rules_owner}` has a non-string `when`")
                                    })
                                    .to_owned()
                            }),
                        });
                    }
                }
                Value::Tagged(reference) => {
                    assert!(reference.tag == "reference", "unknown GitLab YAML tag");
                    let target = reference
                        .value
                        .as_sequence()
                        .expect("GitLab reference target is a sequence");
                    assert_eq!(target.len(), 2, "GitLab reference has two components");
                    assert_eq!(target[1].as_str(), Some("rules"));
                    let target = target[0]
                        .as_str()
                        .expect("GitLab reference owner is a string");
                    if let Some(decision) = self.decision_for_kind(target, kind) {
                        return Some(decision);
                    }
                }
                _ => panic!("`{rules_owner}` has an invalid rule"),
            }
        }
        None
    }
}

struct RuleDecision {
    when: Option<String>,
}

fn yaml(path: impl AsRef<Path>) -> Value {
    let path = path.as_ref();
    let text = fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
    serde_yaml_ng::from_str(&text)
        .unwrap_or_else(|error| panic!("{} is not valid YAML: {error}", path.display()))
}

fn mapping<'a>(value: &'a Value, context: &str) -> &'a Mapping {
    value
        .as_mapping()
        .unwrap_or_else(|| panic!("{context} is not a mapping"))
}

fn pipeline_kind<'a>(condition: &'a Value, owner: &str) -> &'a str {
    condition
        .as_str()
        .and_then(declared_kind)
        .unwrap_or_else(|| panic!("`{owner}` has an unknown rule condition"))
}

/// The pipeline kind a rule condition names, when it names one at all.
fn declared_kind(condition: &str) -> Option<&str> {
    condition
        .strip_prefix("$KITHARA_PIPELINE_KIND == \"")?
        .strip_suffix('"')
}

/// The lane catalog both CI providers read.
fn lane_catalog(root: &Path) -> toml::Table {
    let config: toml::Value = toml::from_str(
        &fs::read_to_string(root.join(".config/xtask.toml")).expect("xtask config is readable"),
    )
    .expect("xtask config is valid TOML");
    config["ext"]["ci"]["lanes"]
        .as_table()
        .expect("CI lanes are a table")
        .clone()
}

fn declared_kinds(lane: &toml::Value) -> BTreeSet<String> {
    lane.get("kinds")
        .and_then(toml::Value::as_array)
        .map(|kinds| {
            kinds
                .iter()
                .filter_map(toml::Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn assert_active_review_job(config: &GitlabConfig, job: &str, expected_owner: &str, judged: bool) {
    let owner = config
        .rules_owner(job)
        .unwrap_or_else(|| panic!("`{job}` has no effective rules"));
    assert_eq!(owner, expected_owner, "`{job}` uses the wrong rules");

    let decision = config
        .decision_for_kind(&owner, MERGE_REQUEST_KIND)
        .unwrap_or_else(|| panic!("`{job}` does not run for merge requests"));
    assert_automatic_when(decision.when.as_deref(), job);

    let effective_when = config.effective_value(job, "when");
    let effective_when = effective_when.as_ref().map(|when| {
        when.as_str()
            .unwrap_or_else(|| panic!("`{job}` has a non-string effective `when`"))
    });
    assert_automatic_when(effective_when, job);
    let allow_failure = config.effective_value(job, "allow_failure");
    if judged {
        assert_eq!(
            allow_failure,
            Some(Value::Bool(true)),
            "`{job}` must report through the blocking verdict"
        );
    } else {
        assert!(allow_failure.is_none(), "`{job}` must remain blocking");
    }
}

fn assert_automatic_when(when: Option<&str>, context: &str) {
    assert!(
        matches!(when, None | Some("on_success")),
        "`{context}` is not an automatic success-gated job"
    );
}

fn assert_exact_rule(rule: &Mapping, condition: &str, when: Option<&str>) {
    let expected_keys = if when.is_some() {
        BTreeSet::from(["if", "when"])
    } else {
        BTreeSet::from(["if"])
    };
    let actual_keys: BTreeSet<&str> = rule
        .keys()
        .map(|key| key.as_str().expect("GitLab rule key is a string"))
        .collect();
    assert_eq!(actual_keys, expected_keys);
    assert_eq!(rule.get("if").and_then(Value::as_str), Some(condition));
    assert_eq!(rule.get("when").and_then(Value::as_str), when);
}

fn workspace_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask has a workspace root")
}

#[test]
fn merge_request_dispatch_is_admitted_without_duplicate_push_pipelines() {
    let dispatch = yaml(workspace_root().join(".gitlab-ci.yml"));
    let dispatch = mapping(&dispatch, "the dispatch pipeline");
    let workflow = mapping(
        dispatch.get("workflow").expect("dispatch has a workflow"),
        "the dispatch workflow",
    );
    let workflow_rules = workflow
        .get("rules")
        .and_then(Value::as_sequence)
        .expect("dispatch workflow rules are a sequence");
    assert!(workflow_rules.len() >= 5);
    assert_exact_rule(
        mapping(&workflow_rules[0], "the merge-request admission rule"),
        "$CI_PIPELINE_SOURCE == \"merge_request_event\"",
        None,
    );
    assert_exact_rule(
        mapping(&workflow_rules[1], "the default-branch push rule"),
        "$CI_PIPELINE_SOURCE == \"push\" && $CI_COMMIT_BRANCH == $CI_DEFAULT_BRANCH",
        None,
    );
    assert_exact_rule(
        mapping(&workflow_rules[2], "the quarantine-push suppression rule"),
        "$CI_PIPELINE_SOURCE == \"push\" && $CI_COMMIT_BRANCH =~ /^quarantine\\//",
        Some("never"),
    );
    assert_exact_rule(
        mapping(
            &workflow_rules[3],
            "the open-merge-request suppression rule",
        ),
        "$CI_PIPELINE_SOURCE == \"push\" && $CI_OPEN_MERGE_REQUESTS",
        Some("never"),
    );
    assert_exact_rule(
        mapping(&workflow_rules[4], "the branch push rule"),
        "$CI_PIPELINE_SOURCE == \"push\"",
        None,
    );

    let review = mapping(
        dispatch
            .get("dispatch:merge-request")
            .expect("dispatch has a merge-request job"),
        "the merge-request dispatch",
    );
    assert_eq!(
        review.get("extends").and_then(Value::as_str),
        Some(".serialized")
    );
    let variables = mapping(
        review
            .get("variables")
            .expect("review dispatch has variables"),
        "the review dispatch variables",
    );
    assert_eq!(
        variables
            .get("KITHARA_PIPELINE_KIND")
            .and_then(Value::as_str),
        Some(MERGE_REQUEST_KIND)
    );
    let review_rules = review
        .get("rules")
        .and_then(Value::as_sequence)
        .expect("review dispatch rules are a sequence");
    assert_eq!(review_rules.len(), 1);
    let review_rule = mapping(&review_rules[0], "the review dispatch rule");
    assert_exact_rule(
        review_rule,
        "$CI_PIPELINE_SOURCE == \"merge_request_event\"",
        None,
    );
    assert!(!review.contains_key("trigger"));
    assert_automatic_when(
        review
            .get("when")
            .map(|when| when.as_str().expect("review dispatch `when` is a string")),
        "dispatch:merge-request",
    );
    assert!(!review.contains_key("allow_failure"));

    let serialized = mapping(
        dispatch
            .get(".serialized")
            .expect("dispatch has a serialized template"),
        "the serialized dispatch template",
    );
    assert_automatic_when(
        serialized.get("when").map(|when| {
            when.as_str()
                .expect("serialized dispatch `when` is a string")
        }),
        ".serialized",
    );
    assert!(!serialized.contains_key("allow_failure"));
    let trigger = mapping(
        serialized
            .get("trigger")
            .expect("serialized dispatch has a trigger"),
        "the serialized trigger",
    );
    assert_eq!(
        trigger.get("strategy").and_then(Value::as_str),
        Some("depend")
    );
    let includes = trigger
        .get("include")
        .and_then(Value::as_sequence)
        .expect("serialized trigger includes child configuration");
    assert!(includes.iter().any(|include| {
        include
            .as_mapping()
            .and_then(|include| include.get("local"))
            .and_then(Value::as_str)
            == Some(".gitlab/ci/pipeline.yml")
    }));
}

#[test]
fn child_pipeline_includes_apple_lanes_and_the_blocking_verdict() {
    let pipeline = yaml(workspace_root().join(".gitlab/ci/pipeline.yml"));
    let pipeline = mapping(&pipeline, "the child pipeline");
    let includes: BTreeSet<&str> = pipeline
        .get("include")
        .and_then(Value::as_sequence)
        .expect("child pipeline includes lane definitions")
        .iter()
        .map(|include| {
            include
                .as_mapping()
                .and_then(|include| include.get("local"))
                .and_then(Value::as_str)
                .expect("child include has a local path")
        })
        .collect();
    assert!(includes.contains(".gitlab/ci/common.yml"));
    assert!(includes.contains(".gitlab/ci/apple.yml"));
    assert!(includes.contains(".gitlab/ci/verdict.yml"));

    let verdict = yaml(workspace_root().join(".gitlab/ci/verdict.yml"));
    let verdict = mapping(&verdict, "the verdict pipeline");
    let verdict = mapping(
        verdict
            .get("verdict")
            .expect("the verdict pipeline has a job"),
        "the verdict job",
    );
    assert_eq!(verdict.get("when").and_then(Value::as_str), Some("always"));
    assert!(!verdict.contains_key("allow_failure"));
}

#[test]
fn judged_jobs_stage_only_checkout_cleaned_verdict_evidence() {
    let root = workspace_root();
    for file in [
        "apple.yml",
        "android.yml",
        "linux.yml",
        "web.yml",
        "verdict.yml",
    ] {
        let document = yaml(root.join(".gitlab/ci").join(file));
        let jobs = mapping(&document, "a CI lane document");
        for (name, definition) in jobs {
            let Some(name) = name.as_str() else {
                continue;
            };
            let Some(paths) = definition
                .as_mapping()
                .and_then(|definition| definition.get("artifacts"))
                .and_then(Value::as_mapping)
                .and_then(|artifacts| artifacts.get("paths"))
                .and_then(Value::as_sequence)
            else {
                continue;
            };
            let verdict_paths = paths
                .iter()
                .filter_map(Value::as_str)
                .filter(|path| path.ends_with("junit/"))
                .collect::<Vec<_>>();
            assert!(
                verdict_paths.iter().all(|path| *path == VERDICT_REPORT_DIR),
                "`{name}` stages verdict evidence outside the checkout-cleaned owner"
            );
        }
    }

    let pipeline = yaml(root.join(".gitlab/ci/pipeline.yml"));
    let variables = mapping(
        mapping(&pipeline, "the child pipeline")
            .get("variables")
            .expect("child pipeline has variables"),
        "the child pipeline variables",
    );
    let clean = variables
        .get("GIT_CLEAN_FLAGS")
        .and_then(Value::as_str)
        .expect("child pipeline defines checkout cleanup");
    assert!(!clean.contains(".ci-artifacts"));
}

#[test]
fn verdict_downloads_every_judged_apple_report() {
    let verdict = yaml(workspace_root().join(".gitlab/ci/verdict.yml"));
    let verdict = mapping(&verdict, "the verdict pipeline");
    let verdict = mapping(
        verdict
            .get("verdict")
            .expect("the verdict pipeline has a job"),
        "the verdict job",
    );
    let verdict_needs: BTreeSet<&str> = verdict
        .get("needs")
        .and_then(Value::as_sequence)
        .expect("the verdict declares judged jobs")
        .iter()
        .map(|need| {
            let need = mapping(need, "a verdict dependency");
            assert_eq!(need.get("artifacts").and_then(Value::as_bool), Some(true));
            assert_eq!(need.get("optional").and_then(Value::as_bool), Some(true));
            need.get("job")
                .and_then(Value::as_str)
                .expect("a verdict dependency names its job")
        })
        .collect();
    for job in [
        "apple:test",
        "apple:test-flash-off",
        "apple:swift-test",
        "apple:ios-test",
    ] {
        assert!(verdict_needs.contains(job), "verdict must judge `{job}`");
    }
}

#[test]
fn an_open_merge_request_runs_the_complete_apple_review_matrix() {
    let config = GitlabConfig::load(workspace_root());
    let expected_jobs = BTreeSet::from([
        "apple:e2e",
        "apple:ios",
        "apple:ios-test",
        "apple:lint",
        "apple:msrv",
        "apple:safari",
        "apple:swift-test",
        "apple:test",
        "apple:test-flash-off",
        "apple:xcframework",
    ]);
    let actual_jobs: BTreeSet<&str> = config
        .job_names()
        .filter(|name| name.starts_with("apple:"))
        .collect();
    assert_eq!(actual_jobs, expected_jobs);

    for (job, owner, judged) in [
        ("apple:lint", ".rules-verify-and-branch", false),
        ("apple:test", ".rules-verify-and-branch", true),
        ("apple:xcframework", ".rules-verify-and-branch", false),
        ("apple:ios", ".rules-verify", false),
        ("apple:ios-test", ".rules-integration-and-review", true),
        ("apple:e2e", ".rules-review-or-nightly", false),
    ] {
        assert_active_review_job(&config, job, owner, judged);
    }
}

#[test]
fn safari_stays_out_of_merge_requests_and_runs_nightly() {
    let config = GitlabConfig::load(workspace_root());
    assert_eq!(
        config.rules_owner("apple:safari").as_deref(),
        Some(".rules-nightly")
    );
    assert!(
        config
            .decision_for_kind(".rules-nightly", MERGE_REQUEST_KIND)
            .is_none()
    );
    let nightly = config
        .decision_for_kind(".rules-nightly", "nightly")
        .expect("Safari runs nightly");
    assert_automatic_when(nightly.when.as_deref(), "apple:safari");
}

// Membership is declared once, in the catalog. A GitLab job that admits a kind
// its lane does not name is the two drifting apart, and the drift shows up as a
// lane running on a pipeline nobody chose.
//
// A lane the catalog does not declare is one `xtask ci run` matches ahead of the
// config - the release and Apple-suite lanes that carry their own arguments, and
// the verdict. Those have no `kinds` to compare against; a name that is neither
// declared nor matched fails the job the moment it runs.
#[test]
fn every_gitlab_lane_job_runs_the_kinds_its_lane_declares() {
    let root = workspace_root();
    let config = GitlabConfig::load(root);
    let lanes = lane_catalog(root);

    let mut checked = 0;
    for (job, lane_name) in config.lane_jobs() {
        let Some(lane) = lanes.get(&lane_name) else {
            continue;
        };
        let admitted = config.admitted_kinds(&job);
        let declared = declared_kinds(lane);
        assert_eq!(
            admitted, declared,
            "job `{job}` admits {admitted:?} but lane `{lane_name}` declares {declared:?}"
        );
        checked += 1;
    }

    assert!(checked > 0, "no GitLab job runs a declared lane");
}

// The other direction. A lane that names the pipelines it belongs to and that no
// job runs is a lane declared into a schedule it never reaches - the failure the
// broadcast lane sat in for months, invisible because nothing compared the two.
// Lanes with no GitLab kinds are GitHub's, and `kinds_github` speaks for them.
#[test]
fn every_lane_that_names_a_gitlab_pipeline_has_a_job_that_runs_it() {
    let root = workspace_root();
    let scheduled: BTreeSet<String> = GitlabConfig::load(root)
        .lane_jobs()
        .into_iter()
        .map(|(_, lane)| lane)
        .collect();

    for (name, lane) in lane_catalog(root) {
        if declared_kinds(&lane).is_empty() {
            continue;
        }
        assert!(
            scheduled.contains(&name),
            "lane `{name}` names GitLab pipelines but no job runs it"
        );
    }
}

/// `GIT_CONFIG_COUNT` is the git binary's protocol and libgit2 does not read
/// it, so pinning the HTTP version alone leaves Cargo's own fetch on the
/// stalling one: `deps:deny` spent twenty-five minutes listing the `boringssl`
/// submodule's refs before its job timed out. The two halves are one
/// workaround and neither carries the other.
#[test]
fn cargo_fetches_through_the_git_binary_that_reads_the_pinned_http_version() {
    let pipeline = yaml(workspace_root().join(".gitlab/ci/pipeline.yml"));
    let variables = mapping(
        mapping(&pipeline, "the child pipeline")
            .get("variables")
            .expect("child pipeline has variables"),
        "the child pipeline variables",
    );

    assert_eq!(
        variables.get("GIT_CONFIG_KEY_0").and_then(Value::as_str),
        Some("http.version"),
        "the pinned HTTP version is what Cargo has to be routed to"
    );
    assert_eq!(
        variables
            .get("CARGO_NET_GIT_FETCH_WITH_CLI")
            .and_then(Value::as_str),
        Some("true"),
        "the pinned HTTP version reaches Cargo only through the git binary"
    );
}
