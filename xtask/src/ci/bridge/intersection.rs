use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;

use crate::{config::CiLaneConfig, consts};

/// What the default branch builds and checks, read from one of its commits.
pub(super) struct Workspace {
    pub(super) members: Vec<Member>,
    /// The lanes each CI job runs, by the bindings of its entrypoint.
    pub(super) jobs: BTreeMap<String, Vec<String>>,
    pub(super) lanes: BTreeMap<String, CiLaneConfig>,
}

/// A workspace member as `cargo metadata` describes it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Member {
    pub(super) name: String,
    /// The manifest directory, relative to the workspace root.
    pub(super) dir: String,
    /// Every package it depends on, in any role.
    pub(super) dependencies: Vec<String>,
}

/// Whether what moved on the default branch reaches what a pull request
/// changed. `members` describe the default branch. Each side's paths map to
/// the member whose manifest directory holds them, and each set grows by every
/// member that depends on it, directly or not; the move reaches the pull
/// request when the two sets meet.
///
/// A move of the CI configuration or of a workspace-wide build input reaches
/// everything. So does a manifest the pull request changes: its dependency
/// edges are not in `members`, and reading them would run cargo on content
/// nobody trusted yet. A pull request's other paths outside every member reach
/// nothing, because no member builds them.
pub(super) fn reaches(main_changed: &[String], pr_changed: &[String], members: &[Member]) -> bool {
    let input = |path: &str| {
        consts::WORKSPACE_INPUTS
            .iter()
            .any(|entry| covers(entry, path))
    };
    let mut main = BTreeSet::new();
    for path in main_changed {
        if input(path)
            || consts::CONTROL_PATHS
                .iter()
                .any(|entry| covers(entry, path))
        {
            return true;
        }
        main.extend(owner(path, members));
    }
    let mut pr = BTreeSet::new();
    for path in pr_changed {
        if input(path) || path.rsplit('/').next() == Some(consts::CARGO_MANIFEST) {
            return true;
        }
        pr.extend(owner(path, members));
    }
    let dependents = dependents(members);
    !close(main, &dependents).is_disjoint(&close(pr, &dependents))
}

/// Whether a move of `moved` paths reaches what a CI job checks. A job that no
/// binding names, or that runs a lane the catalog does not hold, is reached by
/// any move: nothing says what it reads.
pub(super) fn touches(job: &str, moved: &[String], workspace: &Workspace) -> Result<bool> {
    let Some(lanes) = workspace.jobs.get(job) else {
        return Ok(true);
    };
    for name in lanes {
        match workspace.lanes.get(name) {
            Some(lane) if !lane.scheduled_by(name, moved)? => {}
            _ => return Ok(true),
        }
    }
    Ok(false)
}

/// Whether `entry` names `path` itself or, ending in `/`, a directory above it.
fn covers(entry: &str, path: &str) -> bool {
    entry
        .strip_suffix('/')
        .map_or(path == entry, |dir| path.starts_with(entry) || path == dir)
}

/// The member whose manifest directory is the deepest one holding `path`.
fn owner<'a>(path: &str, members: &'a [Member]) -> Option<&'a str> {
    members
        .iter()
        .filter(|member| {
            member.dir.is_empty()
                || path
                    .strip_prefix(member.dir.as_str())
                    .is_some_and(|rest| rest.starts_with('/'))
        })
        .max_by_key(|member| member.dir.len())
        .map(|member| member.name.as_str())
}

/// For every member, the members that depend on it.
fn dependents(members: &[Member]) -> BTreeMap<&str, Vec<&str>> {
    let names: BTreeSet<&str> = members.iter().map(|member| member.name.as_str()).collect();
    let mut dependents: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for member in members {
        for dependency in &member.dependencies {
            if let Some(name) = names.get(dependency.as_str()) {
                dependents.entry(name).or_default().push(&member.name);
            }
        }
    }
    dependents
}

fn close<'a>(
    seeds: BTreeSet<&'a str>,
    dependents: &BTreeMap<&str, Vec<&'a str>>,
) -> BTreeSet<&'a str> {
    let mut reached = seeds.clone();
    let mut pending: Vec<&str> = seeds.into_iter().collect();
    while let Some(name) = pending.pop() {
        for dependent in dependents.get(name).into_iter().flatten() {
            if reached.insert(dependent) {
                pending.push(dependent);
            }
        }
    }
    reached
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(name: &str, dir: &str, dependencies: &[&str]) -> Member {
        Member {
            name: name.to_owned(),
            dir: dir.to_owned(),
            dependencies: dependencies.iter().map(|name| (*name).to_owned()).collect(),
        }
    }

    fn workspace() -> Vec<Member> {
        vec![
            member("kithara-net", "crates/kithara-net", &["url"]),
            member("kithara-stream", "crates/kithara-stream", &["kithara-net"]),
            member("kithara-audio", "crates/kithara-audio", &["kithara-stream"]),
            member("kithara-ui", "crates/kithara-ui", &[]),
        ]
    }

    fn paths(paths: &[&str]) -> Vec<String> {
        paths.iter().map(|path| (*path).to_owned()).collect()
    }

    #[test]
    fn unrelated_members_do_not_meet() {
        assert!(!reaches(
            &paths(&["crates/kithara-ui/src/lib.rs", "docs/guide.md"]),
            &paths(&["crates/kithara-audio/src/lib.rs", "README.md"]),
            &workspace(),
        ));
    }

    #[test]
    fn a_move_reaches_a_pull_request_through_a_reverse_dependency() {
        let pr = paths(&["crates/kithara-audio/src/lib.rs"]);
        assert!(reaches(
            &paths(&["crates/kithara-net/src/http.rs"]),
            &pr,
            &workspace()
        ));
        assert!(
            !reaches(
                &pr,
                &paths(&["crates/kithara-net/src/http.rs"]),
                &[
                    member("kithara-net", "crates/kithara-net", &[]),
                    member("kithara-audio", "crates/kithara-audio", &[]),
                ]
            ),
            "without the dependency the two members do not meet"
        );
    }

    #[test]
    fn a_control_path_or_a_workspace_input_in_the_move_reaches_everything() {
        for moved in [
            ".gitlab/ci/apple.yml",
            "xtask/src/main.rs",
            "Cargo.lock",
            ".cargo/config.toml",
        ] {
            assert!(
                reaches(
                    &paths(&[moved]),
                    &paths(&["crates/kithara-ui/src/lib.rs"]),
                    &workspace()
                ),
                "{moved}"
            );
        }
    }

    /// A manifest the pull request changes may add an edge the default
    /// branch's members do not know, or a member they do not have.
    #[test]
    fn a_manifest_or_a_lockfile_in_the_pull_request_reaches_everything() {
        for changed in [
            "crates/kithara-ui/Cargo.toml",
            "crates/kithara-new/Cargo.toml",
            "Cargo.lock",
        ] {
            assert!(
                reaches(
                    &paths(&["crates/kithara-net/src/http.rs"]),
                    &paths(&[changed]),
                    &workspace()
                ),
                "{changed}"
            );
        }
    }

    #[test]
    fn a_job_is_touched_unless_every_lane_it_runs_names_paths_the_move_missed() {
        let lane = |globs: &[&str]| CiLaneConfig {
            path_globs: globs.iter().map(|glob| (*glob).to_owned()).collect(),
            ..CiLaneConfig::default()
        };
        let workspace = Workspace {
            members: Vec::new(),
            jobs: BTreeMap::from([
                (
                    "deps:advisories".to_owned(),
                    vec!["deps-advisories".to_owned()],
                ),
                ("linux:lint".to_owned(), vec!["linux-lint".to_owned()]),
                (
                    "apple:ios-test".to_owned(),
                    vec!["apple-ios-test".to_owned()],
                ),
            ]),
            lanes: BTreeMap::from([
                ("deps-advisories".to_owned(), lane(&["Cargo.lock"])),
                ("linux-lint".to_owned(), lane(&[])),
            ]),
        };
        let moved = paths(&["crates/kithara-net/src/http.rs"]);

        for (job, touched) in [
            ("deps:advisories", false),
            ("linux:lint", true),
            ("apple:ios-test", true),
            ("verdict", true),
        ] {
            assert_eq!(touches(job, &moved, &workspace).unwrap(), touched, "{job}");
        }
        assert!(touches("deps:advisories", &paths(&["Cargo.lock"]), &workspace).unwrap());
    }

    #[test]
    fn a_path_is_owned_by_the_deepest_manifest_directory() {
        let members = vec![
            member("kithara", "", &[]),
            member("kithara-net", "crates/kithara-net", &[]),
            member("kithara-net-tests", "crates/kithara-net/tests", &[]),
        ];

        assert_eq!(
            owner("crates/kithara-net/tests/a.rs", &members),
            Some("kithara-net-tests")
        );
        assert_eq!(
            owner("crates/kithara-net/src/a.rs", &members),
            Some("kithara-net")
        );
        assert_eq!(owner("crates/kithara-netx/a.rs", &members), Some("kithara"));
    }
}
