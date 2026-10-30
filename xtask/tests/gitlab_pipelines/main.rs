use std::{collections::BTreeSet, fs, path::Path};

use serde_yaml_ng::{Mapping, Value};

mod deps_deny;

const MERGE_REQUEST_KIND: &str = "merge-request";
/// How a job on a host that carries `just` runs a lane.
const JUST_LANE: &str = "just ci run ";
/// How the Windows guest, which carries no `just`, runs one.
const CARGO_LANE: &str = "cargo run --locked -p xtask -- ci run ";
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
        const INVOCATIONS: [&str; 2] = [JUST_LANE, CARGO_LANE];

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
        self.rules_owner(job).map_or_else(BTreeSet::new, |owner| {
            self.admitted_kinds_inner(&owner, &mut Vec::new())
        })
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

    /// A job's variables as the runner hands them to it: the pipeline's, then
    /// each parent's over them, then the job's own. GitLab merges `variables`
    /// key by key across `extends`, where every other key is replaced whole.
    fn variables(&self, name: &str) -> Mapping {
        let mut merged = self.definition("variables").clone();
        self.merge_variables(name, &mut merged, &mut Vec::new());
        merged
    }

    fn merge_variables(&self, name: &str, merged: &mut Mapping, stack: &mut Vec<String>) {
        assert!(
            !stack.iter().any(|parent| parent == name),
            "GitLab inheritance cycle through `{name}`"
        );
        stack.push(name.to_owned());
        for parent in self.extends(name) {
            self.merge_variables(parent, merged, stack);
        }
        if let Some(own) = self
            .definition(name)
            .get("variables")
            .and_then(Value::as_mapping)
        {
            for (key, value) in own {
                merged.insert(key.clone(), value.clone());
            }
        }
        stack.pop();
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
                            matches!(key, "if" | "when" | "allow_failure" | "interruptible"),
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
                            allow_failure: rule.get("allow_failure").map(|allowed| {
                                allowed.as_bool().unwrap_or_else(|| {
                                    panic!("`{rules_owner}` has a non-boolean `allow_failure`")
                                })
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

    /// Whether a job's rules let a pipeline of `kind` in: the first rule that
    /// names this kind, or names none and guards on nothing, decides. A rule
    /// that guards on another variable is a filter that only refuses, so it is
    /// passed over.
    fn admits_kind(&self, rules_owner: &str, kind: &str) -> Option<bool> {
        let rules = self.definition(rules_owner)["rules"]
            .as_sequence()
            .unwrap_or_else(|| panic!("`{rules_owner}` rules are not a sequence"));
        for rule in rules {
            match rule {
                Value::Mapping(rule) => {
                    let decides = rule
                        .get("if")
                        .and_then(Value::as_str)
                        .is_none_or(|condition| declared_kind(condition) == Some(kind));
                    if decides {
                        return Some(rule.get("when").and_then(Value::as_str) != Some("never"));
                    }
                }
                Value::Tagged(reference) => {
                    let target = reference.value[0]
                        .as_str()
                        .expect("GitLab reference owner is a string");
                    if let Some(admitted) = self.admits_kind(target, kind) {
                        return Some(admitted);
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
    allow_failure: Option<bool>,
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
        Some(".dispatch")
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

    let template = mapping(
        dispatch
            .get(".dispatch")
            .expect("dispatch has a shared template"),
        "the dispatch template",
    );
    assert_automatic_when(
        template
            .get("when")
            .map(|when| when.as_str().expect("dispatch template `when` is a string")),
        ".dispatch",
    );
    assert!(!template.contains_key("allow_failure"));
    let trigger = mapping(
        template
            .get("trigger")
            .expect("dispatch template has a trigger"),
        "the template trigger",
    );
    assert_eq!(
        trigger.get("strategy").and_then(Value::as_str),
        Some("depend")
    );
    let includes = trigger
        .get("include")
        .and_then(Value::as_sequence)
        .expect("template trigger includes child configuration");
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
fn the_branch_verdict_expects_only_the_apple_test_producer() {
    let config = GitlabConfig::load(workspace_root());
    let jobs = config.lane_jobs();
    let producers: BTreeSet<&str> = config.definition("verdict")["needs"]
        .as_sequence()
        .expect("verdict needs producers")
        .iter()
        .map(|need| need["job"].as_str().expect("a verdict need names a job"))
        .filter(|job| config.admitted_kinds(job).contains("branch"))
        .map(|job| {
            jobs.iter()
                .find(|(name, _)| name == job)
                .map(|(_, lane)| lane.as_str())
                .expect("a verdict producer runs a lane")
        })
        .collect();
    assert_eq!(producers, BTreeSet::from(["apple-test"]));
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

/// A lane on a host that carries `just` builds in a build root of its own
/// checkout's, beside the checkout rather than in it: the runner cleans the
/// checkout before every job. Cargo is told the root, and the lane runner
/// builds behind the alias inside it. The bootstrap build sits in the same
/// root, and the compiler-cache server is the checkout's, so two jobs on one
/// runner never share one, nor retire the one the other compiles through.
#[test]
fn every_lane_run_through_just_builds_in_a_root_beside_its_own_checkout() {
    const CHECKOUT: &str = "${CI_PROJECT_DIR}";
    let config = GitlabConfig::load(workspace_root());
    let jobs: BTreeSet<String> = config
        .lane_jobs()
        .into_iter()
        .map(|(job, _)| job)
        .filter(|job| {
            config
                .definition(job)
                .get("script")
                .and_then(Value::as_sequence)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .any(|line| line.trim().starts_with(JUST_LANE))
        })
        .collect();
    assert!(!jobs.is_empty(), "no job runs a lane through `just`");

    for job in jobs {
        let variables = config.variables(&job);
        let value = |key: &str| {
            variables
                .get(key)
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("`{job}` names no {key}"))
                .to_owned()
        };
        let root = value("CARGO_TARGET_DIR");
        let bootstrap = value("KITHARA_XTASK_TARGET");
        assert_eq!(
            Path::new(&bootstrap).parent(),
            Some(Path::new(&root)),
            "`{job}` bootstraps outside the root it builds in"
        );
        let sibling = root.strip_prefix(&format!("{CHECKOUT}/../"));
        assert!(
            sibling.is_some_and(|name| !name.is_empty() && name != ".." && !name.contains('/')),
            "`{job}` builds in {root}, not beside its checkout"
        );
        let socket = value("SCCACHE_SERVER_UDS");
        for identity in ["${CI_RUNNER_SHORT_TOKEN}", "${CI_CONCURRENT_PROJECT_ID}"] {
            assert!(
                socket.contains(identity),
                "`{job}` compiles through {socket}, which is not its checkout's own"
            );
        }
    }
}

#[test]
fn macos_review_jobs_can_use_either_native_registration() {
    let config = GitlabConfig::load(workspace_root());
    let registrations = [
        ["kithara-macos", "kithara-macos-1"],
        ["kithara-macos", "kithara-macos-2"],
    ];
    for job in ["apple:lint", "apple:test", "apple:xcframework", "verdict"] {
        let tags = config.effective_value(job, "tags").expect("job has tags");
        let tags = tags.as_sequence().expect("job tags are a sequence");
        assert!(!tags.is_empty(), "`{job}` must select the native pool");
        for registration in registrations {
            assert!(
                tags.iter()
                    .all(|tag| tag.as_str().is_some_and(|tag| registration.contains(&tag))),
                "`{job}` cannot use free registration {registration:?}: {tags:?}"
            );
        }
    }
}

#[test]
fn each_independent_store_has_a_default_branch_source_publisher() {
    let lanes = lane_catalog(workspace_root());
    let gitlab = GitlabConfig::load(workspace_root());
    for (name, os) in [("apple-lint", "macos"), ("linux-lint", "linux")] {
        assert_eq!(lanes[name]["publishes_sources"].as_bool(), Some(true));
        assert_eq!(lanes[name]["os"].as_str(), Some(os));
    }
    assert!(gitlab.admitted_kinds("apple:lint").contains("main"));
    assert!(
        lanes["linux-lint"]["kinds_github"]
            .as_array()
            .unwrap()
            .iter()
            .any(|kind| kind.as_str() == Some("main"))
    );
}

#[test]
fn cache_evidence_uploads_only_the_current_job_even_when_it_fails() {
    let config = GitlabConfig::load(workspace_root());
    let default = &config.definition("default")["artifacts"];
    let target = ".ci-artifacts/cache/gitlab-$CI_JOB_ID-*.json";
    for (name, _) in config.lane_jobs() {
        let artifact = config
            .effective_value(&name, "artifacts")
            .unwrap_or_else(|| default.clone());
        let paths = artifact["paths"].as_sequence().expect("artifact paths");
        assert!(
            paths.iter().any(|path| path.as_str() == Some(target)),
            "{name}: missing current-job cache report"
        );
        assert!(
            !paths
                .iter()
                .any(|path| path
                    .as_str()
                    .is_some_and(|path| path == ".ci-artifacts/cache/"
                        || path == ".ci-artifacts/cache/job.json")),
            "{name}: uploads inherited cache reports"
        );
        assert_eq!(
            artifact["when"].as_str(),
            Some("always"),
            "{name}: loses failure evidence"
        );
    }
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
        // The simulator suite blocks rather than reports: no other host can
        // answer for the iOS surface. A draft is a branch and leaves it to the
        // review that follows.
        ("apple:ios-test", ".rules-verify", false),
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

// The one deep lane a review may ask for by hand. Offered there, it must never
// hold the pipeline: in `rules:`, `manual` blocks unless the rule allows the job
// to fail, and the bridge reads a quarantine child still waiting on a job as a
// failure. The nightly channel keeps running it unasked, and a failure there
// still fails the night.
#[test]
fn the_apple_sanitizer_is_offered_to_review_pipelines_without_blocking_them() {
    let config = GitlabConfig::load(workspace_root());
    assert_eq!(
        config.rules_owner("deep:rtsan").as_deref(),
        Some("deep:rtsan")
    );
    assert_eq!(config.effective_value("deep:rtsan", "allow_failure"), None);
    let nightly = config
        .decision_for_kind("deep:rtsan", "nightly")
        .expect("the nightly channel runs the sanitizer");
    assert_automatic_when(nightly.when.as_deref(), "deep:rtsan");
    assert_eq!(nightly.allow_failure, None);

    for kind in ["branch", MERGE_REQUEST_KIND, "quarantine"] {
        let review = config
            .decision_for_kind("deep:rtsan", kind)
            .unwrap_or_else(|| panic!("a {kind} pipeline is offered the sanitizer"));
        assert_eq!(
            review.when.as_deref(),
            Some("manual"),
            "a {kind} pipeline runs it only when asked"
        );
        assert_eq!(
            review.allow_failure,
            Some(true),
            "and a {kind} pipeline neither waits on it nor fails with it"
        );
    }
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

#[test]
fn semver_audit_blocks_release_builds_but_not_other_pipeline_kinds() {
    let config = GitlabConfig::load(workspace_root());
    assert_eq!(
        config.admitted_kinds("deps:semver"),
        BTreeSet::from(["release".to_owned(), "weekly".to_owned()])
    );

    for job in ["release:xcframework", "release:android"] {
        let needs = config
            .effective_value(job, "needs")
            .expect("release build declares its prerequisites");
        assert!(
            needs
                .as_sequence()
                .is_some_and(|needs| needs.iter().any(|need| {
                    need["job"].as_str() == Some("deps:semver")
                        && need["optional"].as_bool() == Some(true)
                })),
            "{job} must wait for semver when a release schedules it"
        );
    }
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

#[test]
fn dispatch_does_not_reserve_the_host_while_children_run() {
    let document = yaml(workspace_root().join(".gitlab-ci.yml"));
    for (name, definition) in mapping(&document, "dispatch pipeline") {
        let Some(job) = definition.as_mapping() else {
            continue;
        };
        assert!(
            !job.contains_key("resource_group"),
            "{name:?} must leave resource ownership to child jobs"
        );
    }
    let config = GitlabConfig::load(workspace_root());
    for job in ["apple:test", "apple:test-flash-off", "apple:ios-test"] {
        assert_eq!(
            config
                .effective_value(job, "resource_group")
                .as_ref()
                .and_then(Value::as_str),
            Some("kithara-suite"),
            "{job} must retain measurement isolation"
        );
    }
}

/// A run names platforms, or a release version, in the variables it is started
/// with, and only the trigger hands those to the child. A dispatcher that
/// spells one as a job variable naming itself hands down the literal `$NAME`
/// on every run that does not set it, and a rule reads that as a value: a
/// release run naming no platform started the platform matrix beside the
/// release.
/// The bridge starts a draft pull request's run with
/// `KITHARA_QUARANTINE_DRAFT`. It is a branch in what it runs and a quarantine
/// in what it may trust, and the quarantine dispatcher leaves it alone.
#[test]
fn a_draft_pull_request_runs_as_a_branch_under_quarantine_trust() {
    let dispatch = yaml(workspace_root().join(".gitlab-ci.yml"));
    let dispatch = mapping(&dispatch, "the dispatch pipeline");
    let rules = |job: &str| {
        mapping(
            dispatch.get(job).expect("the dispatcher exists"),
            "a dispatcher",
        )
        .get("rules")
        .and_then(Value::as_sequence)
        .expect("dispatcher rules are a sequence")
        .clone()
    };

    let branch = rules("dispatch:branch");
    let draft = mapping(&branch[0], "the draft rule");
    assert_eq!(
        draft.get("if").and_then(Value::as_str),
        Some(
            "$CI_PIPELINE_SOURCE == \"api\" && $KITHARA_QUARANTINE_SHA && \
             $KITHARA_QUARANTINE_DRAFT == \"1\""
        )
    );
    assert_eq!(
        draft["variables"]["KITHARA_CACHE_TRUST"].as_str(),
        Some("quarantine")
    );

    let quarantine = rules("dispatch:quarantine");
    assert_exact_rule(
        mapping(&quarantine[0], "the draft refusal"),
        "$KITHARA_QUARANTINE_DRAFT == \"1\"",
        Some("never"),
    );
}

/// `--touched` measures a branch from the default branch, which on `GitLab` is
/// not `main`.
#[test]
fn a_touched_run_measures_from_the_default_branch() {
    let pipeline = yaml(workspace_root().join(".gitlab/ci/pipeline.yml"));
    assert_eq!(
        pipeline["variables"]["KITHARA_TOUCHED_BASE"].as_str(),
        Some("$CI_DEFAULT_BRANCH")
    );
}

#[test]
fn a_run_variable_reaches_the_child_only_through_the_trigger() {
    let document = yaml(workspace_root().join(".gitlab-ci.yml"));
    for (name, definition) in mapping(&document, "the dispatch pipeline") {
        let name = name.as_str().expect("a dispatch name is a string");
        let variables = if name == "variables" {
            Some(definition)
        } else {
            definition.get("variables")
        };
        for (key, value) in variables.and_then(Value::as_mapping).into_iter().flatten() {
            let key = key.as_str().expect("a variable name is a string");
            let value = value
                .as_str()
                .or_else(|| value.get("value").and_then(Value::as_str))
                .unwrap_or_default();
            assert!(
                !value.contains(&format!("${key}")) && !value.contains(&format!("${{{key}}}")),
                "`{name}` hands `{key}` down as a reference to itself"
            );
        }
    }

    let config = GitlabConfig {
        documents: vec![document.clone()],
    };
    for (name, definition) in mapping(&document, "the dispatch pipeline") {
        let name = name.as_str().expect("a dispatch name is a string");
        if name.starts_with('.') || !definition.is_mapping() {
            continue;
        }
        let Some(trigger) = config.effective_value(name, "trigger") else {
            continue;
        };
        assert_eq!(
            trigger["forward"]["pipeline_variables"].as_bool(),
            Some(true),
            "`{name}` does not hand the run's variables to its child"
        );
    }
}

#[test]
fn superseded_review_checks_are_cancelable_in_the_child_pipeline() {
    let root = workspace_root();
    let document = yaml(root.join(".gitlab/ci/pipeline.yml"));
    assert_eq!(
        document["workflow"]["auto_cancel"]["on_new_commit"].as_str(),
        Some("none")
    );
    let rules = document["workflow"]["rules"]
        .as_sequence()
        .expect("workflow rules");
    assert_eq!(
        rules[0]["if"].as_str(),
        Some("$KITHARA_PIPELINE_KIND == \"branch\" || $KITHARA_PIPELINE_KIND == \"merge-request\"")
    );
    assert_eq!(
        rules[0]["auto_cancel"]["on_new_commit"].as_str(),
        Some("interruptible")
    );
    assert_eq!(rules[1]["when"].as_str(), Some("always"));
    let config = GitlabConfig::load(root);
    // A rule inherits the block's `interruptible` unless it overrides it, and
    // the kinds a review ref runs under are spread over the two blocks that
    // declare them: a branch on `.rules-verify-and-branch`, a merge request on
    // the `.rules-verify` rules it references.
    for (owner, kind) in [
        (".rules-verify-and-branch", "branch"),
        (".rules-verify", "merge-request"),
        (".rules-review-or-nightly", "branch"),
        (".rules-review-or-nightly", "merge-request"),
    ] {
        let block = config.definition(owner);
        let rules = block["rules"].as_sequence().expect("job rules");
        let condition = format!("$KITHARA_PIPELINE_KIND == \"{kind}\"");
        let rule = rules
            .iter()
            .find(|rule| rule.get("if").and_then(Value::as_str) == Some(condition.as_str()))
            .expect("review rule");
        assert_eq!(
            rule["interruptible"]
                .as_bool()
                .or_else(|| block["interruptible"].as_bool()),
            Some(true),
            "{owner}: {kind}"
        );
    }
    // The lanes a review ref never cancels keep saying so on the block itself.
    for owner in [
        ".rules-integration",
        ".rules-nightly",
        ".rules-review-or-nightly",
    ] {
        assert_eq!(
            config.definition(owner)["interruptible"].as_bool(),
            Some(false),
            "non-review jobs retain their policy"
        );
    }
    // `.rules-verify-and-branch` spells the same policy the other way round:
    // the set is cancelable, and the kinds that guard a landed commit opt out
    // rule by rule inside `.rules-verify`.
    assert_eq!(
        config.definition(".rules-verify-and-branch")["interruptible"].as_bool(),
        Some(true)
    );
    let verify = config.definition(".rules-verify")["rules"]
        .as_sequence()
        .expect("job rules");
    for kind in ["main", "nightly", "release"] {
        let condition = format!("$KITHARA_PIPELINE_KIND == \"{kind}\"");
        let rule = verify
            .iter()
            .find(|rule| rule.get("if").and_then(Value::as_str) == Some(condition.as_str()))
            .expect("verify rule");
        assert_eq!(rule["interruptible"].as_bool(), Some(false), "{kind}");
    }
    assert_eq!(
        config.definition(".rules-release")["interruptible"].as_bool(),
        Some(false)
    );
}

/// The same contract on the Mac mini, followed from the door to the job: a run
/// started on the default branch with `KITHARA_PROVISION=1` passes the
/// workflow, one dispatcher turns it into a kind, and that kind carries the
/// roll-out and nothing else. The rules once named `main`, `nightly` and
/// `release`, but the workflow admitted no run that could ask, and a variable
/// the run was started with did not reach the child, so the job could not be
/// started at all.
#[test]
fn a_provisioning_run_on_the_default_branch_reaches_the_mac_roll_out_alone() {
    const DOOR: &str = "($CI_PIPELINE_SOURCE == \"web\" || $CI_PIPELINE_SOURCE == \"api\") && $CI_COMMIT_BRANCH == $CI_DEFAULT_BRANCH && $KITHARA_PROVISION == \"1\"";

    let dispatch = yaml(workspace_root().join(".gitlab-ci.yml"));
    let dispatch = mapping(&dispatch, "the dispatch pipeline");
    let admits_door = |rules: &Value| {
        rules.as_sequence().is_some_and(|rules| {
            rules.iter().any(|rule| {
                rule.get("if").and_then(Value::as_str) == Some(DOOR) && rule.get("when").is_none()
            })
        })
    };
    assert!(
        admits_door(&dispatch["workflow"]["rules"]),
        "the workflow admits the provisioning run"
    );

    let dispatchers: Vec<&Mapping> = dispatch
        .iter()
        .filter(|(name, _)| name.as_str() != Some("workflow"))
        .filter_map(|(_, job)| job.as_mapping())
        .filter(|job| job.get("rules").is_some_and(admits_door))
        .collect();
    assert_eq!(
        dispatchers.len(),
        1,
        "one dispatcher owns the provisioning run"
    );
    let kind = dispatchers[0]["variables"]["KITHARA_PIPELINE_KIND"]
        .as_str()
        .expect("the provisioning dispatcher names its kind");

    let config = GitlabConfig::load(workspace_root());
    assert_eq!(
        config.admitted_kinds("host:provision"),
        BTreeSet::from([kind.to_owned()]),
        "the roll-out runs on the provisioning kind alone"
    );
    let mut beside: Vec<&str> = config
        .job_names()
        .filter(|job| {
            !job.starts_with('.')
                && !matches!(
                    *job,
                    "stages" | "variables" | "workflow" | "default" | "include"
                )
                && *job != "host:provision"
        })
        .filter(|job| {
            // A job with no rules runs in every pipeline.
            config
                .rules_owner(job)
                .is_none_or(|owner| config.admits_kind(&owner, kind).unwrap_or(false))
        })
        .collect();
    beside.sort_unstable();
    assert!(
        beside.is_empty(),
        "a provisioning run carries the roll-out alone, found {beside:?}"
    );

    // A platform request riding the same run would start a second child whose
    // Linux image job moves the tag underneath the roll-out.
    let platforms = dispatch["dispatch:platforms"]["rules"]
        .as_sequence()
        .expect("platform dispatch rules");
    let refusal = platforms
        .iter()
        .position(|rule| {
            rule.get("if").and_then(Value::as_str) == Some("$KITHARA_PROVISION == \"1\"")
        })
        .expect("the platform dispatcher refuses a provisioning run");
    assert_eq!(platforms[refusal]["when"].as_str(), Some("never"));
    assert!(
        platforms[..refusal]
            .iter()
            .all(|rule| rule.get("when").and_then(Value::as_str) == Some("never")),
        "no admitting rule answers before the refusal"
    );
}

#[test]
fn shared_linux_images_use_the_installed_host_tool() {
    let config = GitlabConfig::load(workspace_root());
    let script = config.definition("linux:image")["script"]
        .as_sequence()
        .expect("image maintenance commands");
    let commands: Vec<&str> = script.iter().filter_map(Value::as_str).collect();
    assert_eq!(
        commands,
        [
            "\"$KITHARA_CI_HOST_BINARY\" ci host mac build-linux-image",
            "\"$KITHARA_CI_HOST_BINARY\" ci host mac smoke-linux",
        ],
        "shared images use the deployed binary and its embedded Dockerfile"
    );
}
