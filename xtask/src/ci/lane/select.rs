use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Result, bail};
use clap::{Args, ValueEnum};
use kithara_devtools::Ctx;
use serde::Serialize;

use crate::{
    ci::run::PipelineKind,
    config::{CiLaneArtifact, CiLaneConfig, KitharaExt, LANE_ROLES},
};

/// Which CI system is asking. A lane may answer them differently, because the
/// fleets are different machines with different budgets.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
pub(crate) enum Fleet {
    #[default]
    Github,
    Gitlab,
}

#[derive(Debug, Args)]
pub(crate) struct LanesArgs {
    /// The role whose lanes to render.
    #[arg(long)]
    pub(crate) role: String,
    #[arg(long, value_enum)]
    pub(crate) kind: PipelineKind,
    /// Render only these lanes, whatever their kinds say. This is how a single
    /// subtask is run on its own. A name must be a declared lane, and one this
    /// role owns must reach this fleet; a name another role owns simply leaves
    /// this role empty.
    #[arg(long, value_delimiter = ' ')]
    pub(crate) only: Vec<String>,
    #[arg(long, value_enum, default_value_t = Fleet::Github)]
    pub(crate) fleet: Fleet,
    /// Print one member of the selection rather than the whole object. The CI
    /// image has no `jq`, and a workflow that parses JSON in shell is logic in
    /// YAML.
    #[arg(long, value_enum)]
    pub(crate) field: Option<Field>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum Field {
    Matrix,
    Dependent,
}

#[derive(Debug, Serialize)]
pub(crate) struct Entry {
    pub(crate) lane: String,
    pub(crate) timeout: u32,
    pub(crate) depth: u32,
    pub(crate) artifact: Option<CiLaneArtifact>,
    pub(crate) queue: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) runner: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct Dependent {
    pub(crate) lane: String,
    pub(crate) timeout: u32,
    pub(crate) depth: u32,
    pub(crate) needs: Vec<String>,
    pub(crate) artifact: Option<CiLaneArtifact>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) runner: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct Selection {
    pub(crate) matrix: Vec<Entry>,
    pub(crate) dependent: Vec<Dependent>,
}

/// The kinds this fleet reads: a lane's own answer where it gives one, the
/// shared answer otherwise.
fn membership(lane: &CiLaneConfig, fleet: Fleet) -> &[String] {
    if fleet == Fleet::Github && !lane.kinds_github.is_empty() {
        return &lane.kinds_github;
    }
    &lane.kinds
}

/// Whether this fleet has a machine for the lane at all. The GitHub fan-out
/// puts every lane through `lane.yml`, whose runner labels are the Linux
/// fleet's; its macOS and Windows work runs from workflows of its own, against
/// runner-label variables of their own. A lane that names another operating
/// system is therefore not one GitHub schedules from the catalog, whatever its
/// membership says - rendering it would run a macOS recipe on a Linux runner.
fn reachable(lane: &CiLaneConfig, fleet: Fleet) -> bool {
    fleet != Fleet::Github || lane.os.as_deref() == Some("linux")
}

/// A lane with the asked-for role that this pipeline kind schedules, or that
/// `--only` named directly. A lane with `needs` still has to pass this before
/// it can land in `matrix`; `dependent` below never calls it, because a
/// consumer's own membership is not what admits it.
fn is_asked_for(lane: &CiLaneConfig, name: &str, kind: &str, args: &LanesArgs) -> bool {
    if lane.role != args.role || !reachable(lane, args.fleet) {
        return false;
    }
    if args.only.is_empty() {
        membership(lane, args.fleet)
            .iter()
            .any(|entry| entry == kind)
    } else {
        args.only.iter().any(|only| only == name)
    }
}

pub(crate) fn render(
    lanes: &BTreeMap<String, CiLaneConfig>,
    args: &LanesArgs,
) -> Result<Selection> {
    if !LANE_ROLES.contains(&args.role.as_str()) {
        bail!(
            "`{}` is not a CI lane role; this repository has {}",
            args.role,
            LANE_ROLES.join(", ")
        );
    }
    for name in &args.only {
        if !lanes.contains_key(name) {
            bail!(
                "`{name}` is not a CI lane; this repository has {}",
                lanes.keys().cloned().collect::<Vec<_>>().join(", ")
            );
        }
    }
    let kind = args.kind.name();

    let matrix: Vec<Entry> = lanes
        .iter()
        .filter(|(name, lane)| lane.needs.is_empty() && is_asked_for(lane, name, kind, args))
        .map(|(name, lane)| Entry {
            lane: name.clone(),
            timeout: lane.timeout_minutes,
            depth: lane.fetch_depth,
            artifact: lane.artifact.clone(),
            queue: lane.queue.clone(),
            runner: lane.github_runner.clone(),
        })
        .collect();

    // A lane with `needs` never earns its own place through `--role`/`--kind`
    // or `--only`: it rides in only once a lane it reads from already made the
    // matrix above. Asking for one producer therefore brings its consumer with
    // it, and asking for an unrelated lane does not. That need-presence test is
    // necessary but not sufficient: by kind (empty `--only`), the consumer
    // still has to answer for this fleet's kind the same way a matrix lane
    // would, or a producer this fleet never runs would drag in a consumer it
    // never asked for either. By name (`--only` naming the producer), the
    // consumer's own membership does not gate it, because that is the whole
    // point of asking for one lane by name.
    let present: BTreeSet<&str> = matrix.iter().map(|entry| entry.lane.as_str()).collect();
    let dependent: Vec<Dependent> = lanes
        .iter()
        .filter(|(_, lane)| {
            lane.role == args.role && !lane.needs.is_empty() && reachable(lane, args.fleet)
        })
        .filter(|(_, lane)| {
            lane.needs
                .iter()
                .any(|need| present.contains(need.as_str()))
        })
        .filter(|(_, lane)| {
            !args.only.is_empty()
                || membership(lane, args.fleet)
                    .iter()
                    .any(|entry| entry == kind)
        })
        .map(|(name, lane)| Dependent {
            lane: name.clone(),
            timeout: lane.timeout_minutes,
            depth: lane.fetch_depth,
            needs: lane.needs.clone(),
            artifact: lane.artifact.clone(),
            runner: lane.github_runner.clone(),
        })
        .collect();

    // `--only` is a promise that every name this role owns lands somewhere. A
    // name whose kind or operating system does not match falls out of both
    // `matrix` and `dependent` silently otherwise, and an empty selection
    // would still exit 0 for a caller that asked for something specific.
    //
    // A name another role owns is that role's business, not an error here: the
    // dispatcher hands one `--only` to every role it starts, and the three that
    // do not own the lane have nothing to run rather than something to refuse.
    let landed: BTreeSet<&str> = matrix
        .iter()
        .map(|entry| entry.lane.as_str())
        .chain(dependent.iter().map(|entry| entry.lane.as_str()))
        .collect();
    let missing: Vec<&str> = args
        .only
        .iter()
        .map(String::as_str)
        .filter(|name| !landed.contains(name))
        .filter(|name| lanes[*name].role == args.role)
        .collect();
    if !missing.is_empty() {
        bail!(
            "`{}` selected nothing for role `{}` kind `{}`; check the lane's role, kinds, and operating system",
            missing.join("`, `"),
            args.role,
            kind
        );
    }

    Ok(Selection { matrix, dependent })
}

/// Print one member of a selection alone, or the whole object when the
/// caller did not ask for a member. Split out from `run` so the dispatch
/// itself is testable without a `Ctx`.
fn field(selection: &Selection, field: Option<Field>) -> Result<String> {
    Ok(match field {
        Some(Field::Matrix) => serde_json::to_string(&selection.matrix)?,
        Some(Field::Dependent) => serde_json::to_string(&selection.dependent)?,
        None => serde_json::to_string(selection)?,
    })
}

pub(crate) fn run(args: &LanesArgs, ctx: &Ctx) -> Result<()> {
    let ext = KitharaExt::from_ctx(ctx)?;
    ext.ci.validate()?;
    let selection = render(&ext.ci.lanes, args)?;
    println!("{}", field(&selection, args.field)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ArtifactWhen;

    fn lane(role: &str, kinds: &[&str], needs: &[&str]) -> CiLaneConfig {
        CiLaneConfig {
            role: role.to_owned(),
            kinds: kinds.iter().map(|kind| (*kind).to_owned()).collect(),
            needs: needs.iter().map(|need| (*need).to_owned()).collect(),
            timeout_minutes: 30,
            cache_group: "linux".to_owned(),
            os: Some("linux".to_owned()),
            program: "just".to_owned(),
            ..CiLaneConfig::default()
        }
    }

    fn catalog() -> BTreeMap<String, CiLaneConfig> {
        BTreeMap::from([
            ("linux-lint".to_owned(), lane("gate", &["main"], &[])),
            ("deep-stress".to_owned(), lane("deep", &["nightly"], &[])),
            (
                "deep-stress-report".to_owned(),
                lane("deep", &["nightly"], &["deep-stress"]),
            ),
            ("deep-miri".to_owned(), lane("deep", &["weekly"], &[])),
        ])
    }

    fn args(role: &str, kind: PipelineKind, only: &[&str]) -> LanesArgs {
        LanesArgs {
            role: role.to_owned(),
            kind,
            only: only.iter().map(|name| (*name).to_owned()).collect(),
            fleet: Fleet::Github,
            field: None,
        }
    }

    #[test]
    fn a_role_and_kind_select_the_lanes_that_declare_both() {
        let selection = render(&catalog(), &args("gate", PipelineKind::Main, &[]))
            .expect("the gate role renders");
        let names: Vec<&str> = selection
            .matrix
            .iter()
            .map(|entry| entry.lane.as_str())
            .collect();
        assert_eq!(names, ["linux-lint"]);
        assert!(selection.dependent.is_empty());
    }

    #[test]
    fn a_github_runner_label_follows_the_lane_into_the_matrix() {
        let mut lanes = catalog();
        lanes
            .get_mut("linux-lint")
            .expect("the lane is configured")
            .github_runner = Some("kithara-test-cache".to_owned());

        let selection =
            render(&lanes, &args("gate", PipelineKind::Main, &[])).expect("the gate role renders");

        assert_eq!(
            selection.matrix[0].runner.as_deref(),
            Some("kithara-test-cache")
        );
    }

    // Asking for one lane must bring what reads its artifact, and nothing else.
    #[test]
    fn selecting_one_lane_brings_only_its_own_dependents() {
        let mut catalog = catalog();
        let dependent = catalog
            .get_mut("deep-stress-report")
            .expect("the dependent lane is in the catalog");
        dependent.artifact = Some(CiLaneArtifact {
            name: "quality-report".to_owned(),
            path: "target/consolidated-quality-report.md".to_owned(),
            when: ArtifactWhen::Failure,
        });
        let selection = render(
            &catalog,
            &args("deep", PipelineKind::Nightly, &["deep-stress"]),
        )
        .expect("the stress lane renders");
        let names: Vec<&str> = selection
            .matrix
            .iter()
            .map(|entry| entry.lane.as_str())
            .collect();
        let dependent: Vec<&str> = selection
            .dependent
            .iter()
            .map(|entry| entry.lane.as_str())
            .collect();
        assert_eq!(names, ["deep-stress"]);
        assert_eq!(dependent, ["deep-stress-report"]);
        assert_eq!(
            field(&selection, Some(Field::Dependent)).expect("the dependent field renders"),
            r#"[{"lane":"deep-stress-report","timeout":30,"depth":0,"needs":["deep-stress"],"artifact":{"name":"quality-report","path":"target/consolidated-quality-report.md","when":"failure"}}]"#
        );
    }

    #[test]
    fn a_lane_whose_needs_were_not_selected_is_left_out() {
        let selection = render(&catalog(), &args("deep", PipelineKind::Weekly, &[]))
            .expect("the weekly deep role renders");
        let names: Vec<&str> = selection
            .matrix
            .iter()
            .map(|entry| entry.lane.as_str())
            .collect();
        assert_eq!(names, ["deep-miri"]);
        assert!(selection.dependent.is_empty());
    }

    #[test]
    fn an_only_that_names_no_lane_fails_with_the_lanes_it_could_have_been() {
        let error = render(
            &catalog(),
            &args("deep", PipelineKind::Nightly, &["deep-strss"]),
        )
        .expect_err("a misspelled lane is refused");
        assert!(
            error.to_string().contains("deep-stress"),
            "the error must list the lanes: {error}"
        );
    }

    #[test]
    fn an_only_this_role_owns_that_this_fleet_cannot_reach_is_refused_rather_than_empty() {
        // A name bypasses its lane's kinds, so what is left to refuse is the
        // machine: asking this fleet for a macOS lane must fail rather than
        // return an empty selection a caller reads as success.
        let mut lanes = catalog();
        let mut elsewhere = lane("deep", &["nightly"], &[]);
        elsewhere.os = Some("macos".to_owned());
        lanes.insert("deep-apple".to_owned(), elsewhere);

        let error = render(
            &lanes,
            &args("deep", PipelineKind::Nightly, &["deep-apple"]),
        )
        .expect_err("a lane this fleet cannot reach is refused");
        assert!(
            error.to_string().contains("deep-apple"),
            "the error must name what selected nothing: {error}"
        );
    }

    #[test]
    fn an_only_another_role_owns_renders_empty_instead_of_refusing() {
        // The dispatcher hands one `--only` to every role it starts. Refusing
        // here would turn every request for one lane into three red jobs beside
        // the one that ran it.
        let selection = render(
            &catalog(),
            &args("gate", PipelineKind::Nightly, &["deep-miri"]),
        )
        .expect("a lane another role owns is not this role's to refuse");
        assert!(selection.matrix.is_empty());
        assert!(selection.dependent.is_empty());
    }

    #[test]
    fn an_unknown_role_fails_with_the_roles_it_could_have_been() {
        let error = render(&catalog(), &args("gaet", PipelineKind::Main, &[]))
            .expect_err("a misspelled role is refused");
        assert!(
            error.to_string().contains("gate"),
            "the error must list the roles: {error}"
        );
    }

    #[test]
    fn a_dependent_whose_own_kind_excludes_the_request_does_not_ride_in_on_its_need_alone() {
        // `deep-weekly-report` reads `deep-stress`, which nightly selects, but
        // its own kinds only name `weekly`. The need-presence test alone
        // would wave it through; membership must still gate it because
        // `--only` was not used to name it directly.
        let lanes = BTreeMap::from([
            ("deep-stress".to_owned(), lane("deep", &["nightly"], &[])),
            (
                "deep-weekly-report".to_owned(),
                lane("deep", &["weekly"], &["deep-stress"]),
            ),
        ]);
        let selection = render(&lanes, &args("deep", PipelineKind::Nightly, &[]))
            .expect("the nightly deep role renders");
        let names: Vec<&str> = selection
            .matrix
            .iter()
            .map(|entry| entry.lane.as_str())
            .collect();
        assert_eq!(names, ["deep-stress"]);
        assert!(
            selection.dependent.is_empty(),
            "a consumer outside this kind must not ride in on its need alone: {:?}",
            selection.dependent
        );
    }

    #[test]
    fn a_field_prints_that_member_alone() {
        let selection = render(&catalog(), &args("gate", PipelineKind::Main, &[]))
            .expect("the gate role renders");
        let printed = field(&selection, Some(Field::Matrix)).expect("the matrix field renders");
        assert!(
            printed.starts_with('[') && printed.contains("linux-lint"),
            "a field prints a bare array of that member alone: {printed}"
        );
    }

    // The two fleets buy different amounts of CI. Where a lane says so, the
    // fleet asking is the one answered.
    #[test]
    fn the_github_fleet_reads_its_own_membership_where_a_lane_declares_one() {
        let mut lanes = catalog();
        let deny = lanes
            .get_mut("linux-lint")
            .expect("the lint lane is in the catalog");
        deny.kinds = vec!["weekly".to_owned()];
        deny.kinds_github = vec!["main".to_owned()];

        let github = render(&lanes, &args("gate", PipelineKind::Main, &[]))
            .expect("the GitHub fleet renders");
        assert_eq!(github.matrix.len(), 1);

        let mut gitlab = args("gate", PipelineKind::Main, &[]);
        gitlab.fleet = Fleet::Gitlab;
        let gitlab = render(&lanes, &gitlab).expect("the GitLab fleet renders");
        assert!(gitlab.matrix.is_empty());
    }

    // The GitHub fan-out has one runner pool and it is Linux; its macOS and
    // Windows work runs from workflows with runner labels of their own. A macOS
    // lane rendered here would run its recipe on a Linux machine and report a
    // green gate for a sanitizer that never ran.
    #[test]
    fn the_github_fleet_never_schedules_a_lane_from_another_operating_system() {
        let mut lanes = catalog();
        let mut elsewhere = lane("gate", &["main"], &[]);
        elsewhere.os = Some("macos".to_owned());
        lanes.insert("deep-rtsan".to_owned(), elsewhere);

        let github = render(&lanes, &args("gate", PipelineKind::Main, &[]))
            .expect("the GitHub fleet renders");
        let names: Vec<&str> = github
            .matrix
            .iter()
            .map(|entry| entry.lane.as_str())
            .collect();
        assert_eq!(names, ["linux-lint"]);

        let mut gitlab = args("gate", PipelineKind::Main, &[]);
        gitlab.fleet = Fleet::Gitlab;
        let gitlab = render(&lanes, &gitlab).expect("the GitLab fleet renders");
        assert!(
            gitlab.matrix.iter().any(|entry| entry.lane == "deep-rtsan"),
            "GitLab has the machine and still schedules the lane"
        );
    }
}
