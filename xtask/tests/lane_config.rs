use std::{fs, path::Path};

fn workspace_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask has a workspace root")
}

/// Every test package — the integration-tests root and each package under
/// `tests/crates/` — keeps its own code at `opt-level = 1` under
/// `test-release`, so a release test run optimises the product it exercises
/// and not the test bodies.
#[test]
fn every_test_package_keeps_its_code_at_opt_level_one_under_test_release() {
    let root = workspace_root();
    let manifest: toml::Value = toml::from_str(
        &fs::read_to_string(root.join("Cargo.toml")).expect("workspace manifest is readable"),
    )
    .expect("workspace manifest is valid TOML");
    let overrides = manifest["profile"]["test-release"]["package"]
        .as_table()
        .expect("test-release has package overrides");
    let packages = fs::read_dir(root.join("tests/crates"))
        .expect("read test packages")
        .map(|entry| entry.expect("read test package").path().join("Cargo.toml"))
        .filter(|manifest| manifest.is_file())
        .chain([root.join("tests/Cargo.toml")]);
    for manifest in packages {
        let package: toml::Value = toml::from_str(
            &fs::read_to_string(&manifest).expect("test package manifest is readable"),
        )
        .expect("test package manifest is valid TOML");
        let name = package["package"]["name"]
            .as_str()
            .expect("test package has a name");
        assert_eq!(
            overrides
                .get(name)
                .and_then(|package| package.get("opt-level"))
                .and_then(toml::Value::as_integer),
            Some(1),
            "{name} must keep test code out of opt-level 3"
        );
    }
}

// A browser lane's name is a promise about what ran. The harness reads
// `KITHARA_SELENIUM_BROWSER` and falls back to chrome when nothing sets it, so
// the lane that names a browser has to name it in its own configuration -
// otherwise `--lane=selenium-firefox` reports Firefox and runs Chrome.
#[test]
fn a_selenium_lane_names_its_browser_in_its_own_environment() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask has a workspace root");
    let config: toml::Value = toml::from_str(
        &fs::read_to_string(root.join(".config/xtask.toml")).expect("xtask config is readable"),
    )
    .expect("xtask config is valid TOML");
    let lanes = config["test"]["lanes"]
        .as_table()
        .expect("test lanes are a table");

    let mut checked = 0;
    for (name, lane) in lanes {
        let Some(browser) = name.strip_prefix("selenium-") else {
            continue;
        };
        let named = lane
            .get("env")
            .and_then(|env| env.get("KITHARA_SELENIUM_BROWSER"))
            .and_then(toml::Value::as_str);
        assert_eq!(
            named,
            Some(browser),
            "lane `{name}` must name its browser: KITHARA_SELENIUM_BROWSER"
        );
        checked += 1;
    }

    assert!(checked > 0, "no selenium lane is configured to check");
}

// The coverage bar gates a report that nothing else gates, so it has to be
// enforced where it is declared, and declared once. Two copies is what let the
// lane pass 80 while the recipe defaulted to 80: agreeing by accident, and one
// edit away from gating a local run and CI at different bars. The bar is above
// what the workspace holds today on purpose - the UI surface gets its own tests
// - so this also fixes the number a green lane may not be bought with.
#[test]
fn the_coverage_bar_is_declared_once_and_enforced_where_it_is_declared() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask has a workspace root");
    let recipe = fs::read_to_string(root.join(".config/just/test.just"))
        .expect("the test recipes are readable");

    assert!(
        recipe.contains(r#"COVERAGE_MIN="${COVERAGE_MIN:-80}""#),
        "the coverage bar is declared in .config/just/test.just"
    );
    assert!(
        recipe.contains(r#"--fail-under-lines "$COVERAGE_MIN""#),
        "the declared bar is the one the report is failed under"
    );

    let config: toml::Value = toml::from_str(
        &fs::read_to_string(root.join(".config/xtask.toml")).expect("xtask config is readable"),
    )
    .expect("xtask config is valid TOML");
    let lanes = config["ext"]["ci"]["lanes"]
        .as_table()
        .expect("CI lanes are a table");

    let mut runs_the_recipe = 0;
    for (name, lane) in lanes {
        let steps = lane
            .get("steps")
            .and_then(toml::Value::as_array)
            .map_or(&[][..], Vec::as_slice);
        for step in steps {
            assert!(
                step.get("env")
                    .and_then(|env| env.get("COVERAGE_MIN"))
                    .is_none(),
                "lane `{name}` keeps a second copy of the bar instead of inheriting it"
            );
            let args: Vec<&str> = step
                .get("args")
                .and_then(toml::Value::as_array)
                .map_or(&[][..], Vec::as_slice)
                .iter()
                .filter_map(toml::Value::as_str)
                .collect();
            if args == ["test", "coverage"] {
                runs_the_recipe += 1;
            }
        }
    }

    assert_eq!(
        runs_the_recipe, 1,
        "exactly one lane runs the coverage recipe that carries the bar"
    );
}

// The catalog validates pipeline kinds by name, the way it already validates
// cache groups by name. Two lists that must agree are one edit away from
// disagreeing silently, so the enum the executor reads and the names the
// catalog accepts are checked against each other here.
#[test]
fn the_catalog_accepts_exactly_the_pipeline_kinds_the_executor_knows() {
    let listed: Vec<String> = xtask_pipeline_kind_names();
    assert_eq!(
        listed,
        vec![
            "branch",
            "platforms",
            "merge-request",
            "quarantine",
            "main",
            "nightly",
            "weekly",
            "release",
        ],
        "PIPELINE_KINDS and PipelineKind must name the same kinds"
    );
}

fn xtask_pipeline_kind_names() -> Vec<String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask has a workspace root");
    let source = fs::read_to_string(root.join("xtask/src/consts.rs"))
        .expect("the xtask constants source is readable");
    let start = source
        .find("pub(crate) const PIPELINE_KINDS")
        .expect("PIPELINE_KINDS is declared");
    let body = &source[start..];
    // Skip past the `: [&str; 8]` type annotation first, so the brackets found
    // below are the array literal's, not the type's.
    let assigned = body.find('=').expect("PIPELINE_KINDS is assigned a value");
    let body = &body[assigned..];
    let open = body.find('[').expect("PIPELINE_KINDS is an array");
    let close = body.find(']').expect("PIPELINE_KINDS array is closed");
    body[open + 1..close]
        .split(',')
        .map(|entry| entry.trim().trim_matches('"').to_owned())
        .filter(|entry| !entry.is_empty())
        .collect()
}

fn xtask_lane_role_names() -> Vec<String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask has a workspace root");
    let source = fs::read_to_string(root.join("xtask/src/consts.rs"))
        .expect("the xtask constants source is readable");
    let start = source
        .find("pub(crate) const LANE_ROLES")
        .expect("LANE_ROLES is declared");
    let body = &source[start..];
    // Skip past the `: [&str; 5]` type annotation first, so the brackets found
    // below are the array literal's, not the type's.
    let assigned = body.find('=').expect("LANE_ROLES is assigned a value");
    let body = &body[assigned..];
    let open = body.find('[').expect("LANE_ROLES is an array");
    let close = body.find(']').expect("LANE_ROLES array is closed");
    body[open + 1..close]
        .split(',')
        .map(|entry| entry.trim().trim_matches('"').to_owned())
        .filter(|entry| !entry.is_empty())
        .collect()
}

// Every command a GitHub workflow runs has to exist as a lane before the
// workflows can stop restating them. This names the lanes that must be there.
#[test]
fn the_catalog_declares_every_lane_the_github_workflows_will_ask_for() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask has a workspace root");
    let config: toml::Value = toml::from_str(
        &fs::read_to_string(root.join(".config/xtask.toml")).expect("xtask config is readable"),
    )
    .expect("xtask config is valid TOML");
    let lanes = config["ext"]["ci"]["lanes"]
        .as_table()
        .expect("CI lanes are a table");

    for name in [
        "linux-lint",
        "linux-arch",
        "linux-msrv",
        "linux-perf-memory",
        "linux-test-real-clock",
        "linux-support",
        "deep-rtsan-fast",
        "deep-rtsan-file",
        "deep-rtsan-hls",
        "deep-ui",
        "deep-miri",
        "deep-fuzz",
        "quality-assess",
        "quality-similarity",
        "quality-architecture",
        "quality-coverage-risk",
        "quality-health",
        "quality-report",
    ] {
        assert!(lanes.contains_key(name), "lane `{name}` must be declared");
    }
}

// The rtsan lanes exist to run the instrumented allocator under load; a lane
// that silently falls back to the harness's default backend would still be
// green while checking nothing new. `quality-report`'s `--artifacts` value is
// the one place a downstream dispatcher agrees with this catalog on where
// artifacts land - drift there is silent, because `ci_report.rs` treats a
// missing directory as "nothing to report" rather than an error.
#[test]
fn every_deep_rtsan_lane_names_its_backend_and_quality_report_names_its_artifacts_path() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask has a workspace root");
    let config: toml::Value = toml::from_str(
        &fs::read_to_string(root.join(".config/xtask.toml")).expect("xtask config is readable"),
    )
    .expect("xtask config is valid TOML");
    let lanes = config["ext"]["ci"]["lanes"]
        .as_table()
        .expect("CI lanes are a table");

    let mut checked = 0;
    for (name, lane) in lanes {
        if !name.starts_with("deep-rtsan-") {
            continue;
        }
        let steps = lane
            .get("steps")
            .and_then(toml::Value::as_array)
            .map_or(&[][..], Vec::as_slice);
        for step in steps {
            let backend = step
                .get("env")
                .and_then(|env| env.get("KITHARA_RTSAN_BACKEND"))
                .and_then(toml::Value::as_str);
            assert!(
                backend.is_some(),
                "lane `{name}` must name its backend: KITHARA_RTSAN_BACKEND"
            );
        }
        checked += 1;
    }
    assert!(checked > 0, "no deep-rtsan-* lane is configured to check");

    let report = lanes
        .get("quality-report")
        .expect("quality-report lane is declared");
    let steps = report
        .get("steps")
        .and_then(toml::Value::as_array)
        .expect("quality-report has steps");
    let mut found = false;
    for step in steps {
        let args: Vec<&str> = step
            .get("args")
            .and_then(toml::Value::as_array)
            .map_or(&[][..], Vec::as_slice)
            .iter()
            .filter_map(toml::Value::as_str)
            .collect();
        let Some(position) = args.iter().position(|arg| *arg == "--artifacts") else {
            continue;
        };
        assert_eq!(
            args.get(position + 1),
            Some(&"{root}/artifacts"),
            "quality-report's --artifacts must be {{root}}/artifacts"
        );
        found = true;
    }
    assert!(found, "quality-report must pass --artifacts to a step");
}

// A lane naming a kind that no pipeline can be is a lane that never runs, and
// nothing else would say so.
#[test]
fn every_declared_lane_names_a_known_role_and_known_kinds() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask has a workspace root");
    let config: toml::Value = toml::from_str(
        &fs::read_to_string(root.join(".config/xtask.toml")).expect("xtask config is readable"),
    )
    .expect("xtask config is valid TOML");
    let lanes = config["ext"]["ci"]["lanes"]
        .as_table()
        .expect("CI lanes are a table");
    let kinds = xtask_pipeline_kind_names();
    let roles = xtask_lane_role_names();

    for (name, lane) in lanes {
        let role = lane
            .get("role")
            .and_then(toml::Value::as_str)
            .unwrap_or_else(|| panic!("lane `{name}` must name a role"));
        assert!(
            roles.contains(&role.to_owned()),
            "lane `{name}` has unknown role `{role}`"
        );
        for field in ["kinds", "kinds_github"] {
            let Some(listed) = lane.get(field).and_then(toml::Value::as_array) else {
                continue;
            };
            for kind in listed {
                let kind = kind.as_str().expect("a kind is a string");
                assert!(
                    kinds.contains(&kind.to_owned()),
                    "lane `{name}`.{field} names unknown kind `{kind}`"
                );
            }
        }
    }
}

/// The kinds a lane enters the GitHub fan-out under: its own `kinds_github`
/// where it is declared, the shared `kinds` otherwise, and none at all for a lane
/// that cannot run on Linux, because that fan-out has only Linux machines.
fn github_membership(lane: &toml::Value) -> Vec<&str> {
    let os = lane.get("os").map_or_else(Vec::new, strings);
    if !os.contains(&"linux") {
        return Vec::new();
    }
    lane.get("kinds_github")
        .or_else(|| lane.get("kinds"))
        .map_or_else(Vec::new, strings)
}

fn strings(value: &toml::Value) -> Vec<&str> {
    match value {
        toml::Value::String(one) => vec![one.as_str()],
        toml::Value::Array(many) => many.iter().filter_map(toml::Value::as_str).collect(),
        _ => Vec::new(),
    }
}

fn ci_lanes(config: &toml::Value) -> &toml::Table {
    config["ext"]["ci"]["lanes"]
        .as_table()
        .expect("the catalog declares lanes")
}

fn xtask_config() -> toml::Value {
    toml::from_str(
        &fs::read_to_string(workspace_root().join(".config/xtask.toml"))
            .expect("xtask config is readable"),
    )
    .expect("xtask config is valid TOML")
}

#[test]
fn both_hosts_restore_lint_builds_after_local_eviction() {
    let config = xtask_config();
    for name in ["apple-lint", "linux-lint"] {
        assert_eq!(
            ci_lanes(&config)[name]
                .get("target_snapshot")
                .and_then(toml::Value::as_str),
            Some(name),
            "{name} must restore build-script output as well as compiler entries"
        );
    }
}

/// On GitHub a push with a pull request ready for review is a `merge-request`
/// pipeline and any other push a `branch` one. They differ in how much of the
/// suite a lane runs, never in which lanes run: a ready pull request that also
/// started what only the default branch pays for would make every review wait
/// on suites no merge request needs.
#[test]
fn a_ready_pull_request_schedules_exactly_the_lanes_a_branch_push_does() {
    let config = xtask_config();
    for (name, lane) in ci_lanes(&config) {
        let kinds = github_membership(lane);
        assert_eq!(
            kinds.contains(&"branch"),
            kinds.contains(&"merge-request"),
            "lane `{name}` runs on GitHub for one of a branch push and a ready pull request but not the other: {kinds:?}"
        );
    }
}

/// The product suite is what a `test run` naming no lane runs. Every lane a
/// branch push schedules for it runs one command, apart from the toggles it
/// asks for, so the clocks it is run under are the only difference between
/// their counts. Only a branch push narrows it to the lanes the push touched;
/// a ready pull request runs it whole, as the default branch does.
#[test]
fn the_product_suite_lanes_differ_only_in_their_toggles_and_narrow_only_on_a_branch_push() {
    let config = xtask_config();
    let mut recipes = Vec::new();
    for (name, lane) in ci_lanes(&config) {
        if !github_membership(lane).contains(&"branch") {
            continue;
        }
        let steps = lane
            .get("steps")
            .and_then(toml::Value::as_array)
            .map_or(&[][..], Vec::as_slice);
        for step in steps {
            let args = step.get("args").map_or_else(Vec::new, strings);
            if !matches!(args.as_slice(), ["test", "run", ..])
                || args.iter().any(|arg| arg.starts_with("--lane"))
            {
                continue;
            }
            assert!(
                !args.contains(&"--touched"),
                "lane `{name}` narrows the suite on kinds other than a branch push"
            );
            let by_kind = step
                .get("args_by_kind")
                .and_then(toml::Value::as_table)
                .cloned()
                .unwrap_or_default();
            assert_eq!(
                by_kind.keys().map(String::as_str).collect::<Vec<_>>(),
                ["branch"],
                "lane `{name}` runs the suite whole on every kind but a branch push"
            );
            let branch = strings(&by_kind["branch"]);
            assert!(
                branch.contains(&"--touched"),
                "lane `{name}` runs the whole suite on a branch push"
            );
            assert_eq!(
                branch
                    .iter()
                    .copied()
                    .filter(|arg| {
                        *arg != "--touched"
                            && !(name == "apple-test"
                                && *arg
                                    == "--selection-output=.ci-artifacts/junit/apple-test.selection.json")
                    })
                    .collect::<Vec<_>>(),
                args,
                "lane `{name}` narrows a different command than it runs whole"
            );
            let command: Vec<&str> = args
                .iter()
                .copied()
                .filter(|arg| !arg.starts_with("--flash=") && !arg.starts_with("--no-block="))
                .collect();
            let env = step
                .get("env")
                .and_then(toml::Value::as_table)
                .cloned()
                .unwrap_or_default();
            let tools = lane.get("tools").map_or_else(Vec::new, strings);
            let history = lane
                .get("history")
                .and_then(toml::Value::as_bool)
                .unwrap_or(false);
            recipes.push((name, (command, env, tools, history)));
        }
    }
    let Some(((first, recipe), rest)) = recipes.split_first() else {
        panic!("no lane a branch push schedules runs the product suite");
    };
    assert!(
        !rest.is_empty(),
        "the product suite runs under one clock only"
    );
    for (name, other) in rest {
        assert_eq!(
            other, recipe,
            "lanes `{first}` and `{name}` build or run the product suite differently"
        );
    }
}

#[test]
fn github_membership_keeps_an_explicit_empty_list_distinct_from_absence() {
    for (declaration, expected) in [
        ("", vec!["weekly"]),
        ("kinds_github = []", Vec::new()),
        (
            "kinds_github = [\"main\", \"nightly\"]",
            vec!["main", "nightly"],
        ),
    ] {
        let lane: toml::Value = toml::from_str(&format!(
            "os = \"linux\"\nkinds = [\"weekly\"]\n{declaration}\n"
        ))
        .expect("the membership fixture parses");
        assert_eq!(github_membership(&lane), expected);
    }
}
