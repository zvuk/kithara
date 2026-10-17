//! The default campaign preserves the audio engine's domain and feature
//! coverage. Support and application lanes belong to ordinary CI.

use std::{
    collections::{BTreeMap, BTreeSet},
    process::Command,
};

use cargo_metadata::Metadata;

use crate::{
    common::project::{ProjectConfig, TestCargoOptions, TestLaneConfig, TestRunner},
    test::{
        ResolvedLane,
        repository_tests::{root, selected_by, this_workspace},
        resolve, toggled,
    },
};

/// Each package `cargo tree` resolves, with every feature set it builds the
/// package with.
type Build = BTreeMap<String, Vec<BTreeSet<String>>>;

/// Ordinary lanes that own engine contracts, including instrumented builds.
/// Mixed integration and broadcast selections have their own stress lanes and
/// pinned-nextest selection contracts.
const ENGINE_DOMAINS: &[&str] = &[
    "abr",
    "analysis",
    "assets",
    "audio",
    "core",
    "decode",
    "dsp",
    "effects",
    "encode",
    "file",
    "hls",
    "host",
    "net",
    "net-host",
    "play",
    "queue",
    "render",
    "storage",
    "sync",
    "warp",
    "usdt-warp",
    "usdt-render-scheduler",
    "usdt-hls",
    "usdt-hls-stress",
    "usdt-queue",
];

/// The packages and features cargo resolves for a lane's build, read from
/// `cargo tree` over the lane's own selection and features. The output is
/// parsed, so it never inherits a forced colour: a runner that sets
/// `CARGO_TERM_COLOR=always` wraps the duplicate marker in escapes, and the
/// marker then reads as a feature name.
fn build_of(lane: &ResolvedLane) -> Build {
    let cargo = std::env::var_os("CARGO").expect("the test runner names the cargo it uses");
    let mut command = Command::new(cargo);
    command.current_dir(root()).args([
        "tree",
        "--color",
        "never",
        "--prefix",
        "none",
        "--format",
        "{p}|{f}",
        "--edges",
        "normal,build,dev",
    ]);
    if lane.cargo.workspace {
        command.arg("--workspace");
        for package in &lane.cargo.exclude {
            command.args(["--exclude", package]);
        }
    }
    for package in &lane.cargo.packages {
        command.args(["--package", package]);
    }
    if !lane.features.is_empty() {
        command.args(["--features", &lane.features.join(",")]);
    }
    let output = command.output().expect("run cargo tree");
    assert!(
        output.status.success(),
        "cargo tree for lane `{}`: {}",
        lane.lane,
        String::from_utf8_lossy(&output.stderr)
    );
    let mut build = Build::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let line = line.strip_suffix(" (*)").unwrap_or(line);
        if let Some((package, features)) = line.split_once('|') {
            let features = features
                .split(',')
                .filter(|feature| !feature.is_empty())
                .map(str::to_owned)
                .collect();
            build.entry(package.to_owned()).or_default().push(features);
        }
    }
    build
}

/// The first package `own` builds that `theirs` does not build with at least
/// the same features, or `None` when `theirs` covers all of `own`.
fn first_uncovered<'a>(own: &'a Build, theirs: &Build) -> Option<(&'a str, &'a BTreeSet<String>)> {
    own.iter().find_map(|(package, sets)| {
        sets.iter()
            .find(|features| {
                !theirs
                    .get(package)
                    .is_some_and(|more| more.iter().any(|other| features.is_subset(other)))
            })
            .map(|features| (package.as_str(), features))
    })
}

/// `theirs` builds every target `own` builds: a lane narrowed to its library
/// or to named test targets runs part of what an unnarrowed one runs.
fn targets_within(own: &TestCargoOptions, theirs: &TestCargoOptions) -> bool {
    let narrows = |cargo: &TestCargoOptions| cargo.lib || !cargo.tests.is_empty();
    !narrows(theirs)
        || (narrows(own)
            && (!own.lib || theirs.lib)
            && own.tests.iter().all(|test| theirs.tests.contains(test)))
}

/// Whether `stressed` runs every test `lane` runs, features aside: both under
/// nextest at the campaign's thread count, the same Cargo profile, at least
/// the same environment, and a selection and filter no narrower. Core's
/// deterministic PCM input generators are support, not an engine contract.
fn runs_within(
    name: &str,
    lane: &TestLaneConfig,
    stressed_name: &str,
    stressed: &TestLaneConfig,
    metadata: &Metadata,
) -> bool {
    let (TestRunner::Nextest(own), TestRunner::Nextest(theirs)) = (&lane.runner, &stressed.runner)
    else {
        return false;
    };
    let filter_covers = theirs.filter.is_none()
        || theirs.filter == own.filter
        || (stressed_name == "engine" && ENGINE_DOMAINS.contains(&name));
    let mut selected = selected_by(&lane.cargo, metadata);
    if name == "core" {
        selected.remove("kithara-core-test-fixtures");
    }
    own.test_threads.is_none()
        && filter_covers
        && (theirs.ignore_default_filter || !own.ignore_default_filter)
        && lane.cargo.profile == stressed.cargo.profile
        && lane
            .env
            .iter()
            .all(|(key, value)| stressed.env.get(key) == Some(value))
        && !selected.is_empty()
        && selected.is_subset(&selected_by(&stressed.cargo, metadata))
        && targets_within(&lane.cargo, &stressed.cargo)
}

#[test]
fn removing_any_engine_domain_package_breaks_test_coverage() {
    let project = ProjectConfig::load(&root()).expect("repository config");
    let metadata = this_workspace();
    for name in ENGINE_DOMAINS {
        let domain = &project.test.lanes[*name];
        assert!(runs_within(
            name,
            domain,
            "engine",
            &project.test.lanes["engine"],
            &metadata
        ));
        for package in &domain.cargo.packages {
            if *name == "core" && package == "kithara-core-test-fixtures" {
                continue;
            }
            let mut stressed = project.test.lanes["engine"].clone();
            stressed
                .cargo
                .packages
                .retain(|selected| selected != package);
            assert!(
                !runs_within(name, domain, "engine", &stressed, &metadata),
                "{name} lost {package}'s tests even if a dependency still builds it"
            );
        }
    }
}

#[test]
fn only_cores_known_pcm_fixture_package_is_outside_engine_coverage() {
    let project = ProjectConfig::load(&root()).expect("repository config");
    let metadata = this_workspace();
    let stressed = &project.test.lanes["engine"];
    let mut core = project.test.lanes["core"].clone();
    assert!(
        core.cargo
            .packages
            .iter()
            .any(|package| package == "kithara-core-test-fixtures")
    );
    assert!(
        !stressed
            .cargo
            .packages
            .iter()
            .any(|package| package == "kithara-core-test-fixtures")
    );
    assert!(runs_within("core", &core, "engine", stressed, &metadata));
    core.cargo.packages.push("kithara-test-utils".to_owned());
    assert!(!runs_within("core", &core, "engine", stressed, &metadata));
    assert!(!runs_within(
        "audio",
        &project.test.lanes["core"],
        "engine",
        stressed,
        &metadata
    ));
}

/// The builds of `lanes`, resolved side by side: each `cargo tree` waits on
/// the resolver for seconds, and the rule reads dozens.
fn builds_of(lanes: &[ResolvedLane]) -> Vec<Build> {
    let workers = std::thread::available_parallelism().map_or(1, usize::from);
    std::thread::scope(|scope| {
        let handles = lanes
            .chunks(lanes.len().div_ceil(workers).max(1))
            .map(|chunk| scope.spawn(move || chunk.iter().map(build_of).collect::<Vec<_>>()))
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .flat_map(|handle| handle.join().expect("a cargo tree worker finishes"))
            .collect()
    })
}

/// Each of `candidates` that no lane of `stressed` repeats with at least its
/// features, under some default lane mode, with the reasons it is not.
fn uncovered<'a>(
    project: &ProjectConfig,
    metadata: &Metadata,
    candidates: &[&'a str],
    stressed: &[&str],
) -> Vec<(&'a str, Vec<String>)> {
    let (test, stress) = (&project.test, &project.stress);
    let resolved = |lane: &str, flash: Option<bool>, no_block: Option<bool>, load: Option<bool>| {
        resolve(
            test,
            &toggled(test, lane, flash, no_block, load).expect("lane"),
        )
        .expect("resolve")
    };
    let candidates = candidates
        .iter()
        .filter(|name| !stressed.contains(name))
        .map(|&name| {
            let lane = &test.lanes[name];
            let within = stressed
                .iter()
                .copied()
                .filter(|other| runs_within(name, lane, other, &test.lanes[*other], metadata))
                .collect::<Vec<_>>();
            (name, within)
        })
        .collect::<Vec<_>>();
    let mut lanes = Vec::new();
    let mut own = BTreeMap::new();
    for (name, within) in &candidates {
        if !within.is_empty() {
            own.insert(*name, lanes.len());
            lanes.push(resolved(name, None, None, None));
        }
    }
    let mut units = BTreeMap::<&str, Vec<(&str, usize)>>::new();
    for other in candidates.iter().flat_map(|(_, within)| within) {
        if units.contains_key(other) {
            continue;
        }
        let mut indices = Vec::new();
        for name in &stress.default_modes {
            let mode = &stress.modes[name];
            if mode.command.is_empty() {
                indices.push((name.as_str(), lanes.len()));
                lanes.push(resolved(other, mode.flash, mode.no_block, mode.load));
            }
        }
        units.insert(other, indices);
    }
    let builds = builds_of(&lanes);
    let mut gaps = Vec::new();
    for (name, within) in &candidates {
        let lane = own.get(name).map(|&index| &builds[index]);
        let covered = within.iter().any(|other| {
            lane.is_some_and(|lane| {
                units[other]
                    .iter()
                    .any(|&(_, unit)| first_uncovered(lane, &builds[unit]).is_none())
            })
        });
        if covered {
            continue;
        }
        let mut why = Vec::new();
        for other in within {
            for &(mode, unit) in &units[other] {
                if let Some((package, features)) =
                    lane.and_then(|lane| first_uncovered(lane, &builds[unit]))
                {
                    why.push(format!(
                        "`{other}` under `{mode}` builds no `{package}` with {features:?}"
                    ));
                }
            }
        }
        if why.is_empty() {
            why.push(
                "no stress lane runs it under the same runner, profile, environment, \
                 selection and targets"
                    .to_owned(),
            );
        }
        gaps.push((*name, why));
    }
    gaps
}

#[test]
fn every_engine_domain_is_stressed_or_covered_with_its_features() {
    let project = ProjectConfig::load(&root()).expect("load repository config");
    let metadata = this_workspace();
    let stress = &project.stress;
    let mut failures = Vec::new();
    if stress.default_filter != "all()" {
        failures.push(format!(
            "stress.default_filter `{}` leaves part of every stressed lane unrepeated",
            stress.default_filter
        ));
    }
    let stressed = stress
        .tiers
        .values()
        .flat_map(|tier| tier.lanes.iter().map(String::as_str))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    for (name, why) in uncovered(&project, &metadata, ENGINE_DOMAINS, &stressed) {
        failures.push(format!(
            "engine domain lane `{name}` is in no stress tier and no stress lane \
             preserves its engine tests and features: {}",
            why.join("; ")
        ));
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// A tier that answers for every lane repeats each lane the project does not
/// exempt, either itself or through a lane that runs all of its tests with at
/// least its features.
#[test]
fn a_tier_covering_every_lane_repeats_each_unexempt_lane_or_a_lane_covering_it() {
    let project = ProjectConfig::load(&root()).expect("load repository config");
    let metadata = this_workspace();
    let stress = &project.stress;
    let lanes = project
        .test
        .lanes
        .keys()
        .map(String::as_str)
        .filter(|name| !stress.not_stressed.contains_key(*name))
        .collect::<Vec<_>>();
    let tiers = stress
        .tiers
        .iter()
        .filter(|(_, tier)| tier.covers_every_lane)
        .collect::<Vec<_>>();
    assert!(!tiers.is_empty(), "no stress tier covers every lane");
    let mut failures = Vec::new();
    for (tier_name, tier) in tiers {
        let stressed = tier.lanes.iter().map(String::as_str).collect::<Vec<_>>();
        for (name, why) in uncovered(&project, &metadata, &lanes, &stressed) {
            failures.push(format!(
                "lane `{name}` is neither in stress tier `{tier_name}` nor repeated by one \
                 of its lanes: {}",
                why.join("; ")
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
