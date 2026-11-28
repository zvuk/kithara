use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    process::Command,
};

use anyhow::{Result, bail};
use clap::{Args, ValueEnum};
use kithara_devtools::Ctx;
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::{
    ci::run::PipelineKind,
    config::{CiLaneArtifact, CiLaneConfig, KitharaExt},
    consts,
};

/// Which CI system is asking. A lane may answer them differently, because the
/// fleets are different machines with different budgets.
#[derive(
    Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize, ValueEnum,
)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Fleet {
    #[default]
    Github,
    Gitlab,
}

/// Which provider runs the catalog. `split` gives each lane to the providers
/// its own lists name, so load is spread over every pool. The other two give
/// one provider every lane either list names and the other none, for when a
/// provider has lost its executors.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
pub(crate) enum Profile {
    #[default]
    Split,
    GitlabOnly,
    GithubOnly,
}

#[derive(Clone, Debug, Args)]
pub(crate) struct LanesArgs {
    /// The role whose lanes to render.
    #[arg(long)]
    pub(crate) role: String,
    #[arg(long, value_enum)]
    pub(crate) kind: PipelineKind,
    /// Render only these named lanes, whatever their kinds say. This is how a
    /// single subtask is run on its own. `all` renders a full run: every lane
    /// some full kind schedules. A lane with no kinds belongs to a dedicated
    /// workflow and leaves this fan-out empty.
    #[arg(long, value_delimiter = ' ')]
    pub(crate) only: Vec<String>,
    #[arg(long, value_enum, default_value_t = Fleet::Github)]
    pub(crate) fleet: Fleet,
    #[arg(long, value_enum, env = "KITHARA_CI_PROFILE", default_value_t = Profile::Split)]
    pub(crate) profile: Profile,
    /// Operating systems this provider has an enabled pool for, in the order
    /// it prefers them. A lane on no listed system is not rendered, rather
    /// than queued for a runner that is not there.
    #[arg(
        long = "pool",
        value_delimiter = ',',
        default_values_t = ["linux", "macos", "windows"].map(String::from)
    )]
    pub(crate) pools: Vec<String>,
    /// Print one member of the selection rather than the whole object. The CI
    /// image has no `jq`, and a workflow that parses JSON in shell is logic in
    /// YAML.
    #[arg(long, value_enum)]
    pub(crate) field: Option<Field>,
    /// Revision the reviewed change is compared against. An unreadable
    /// comparison selects every path-gated lane rather than none.
    #[arg(long)]
    pub(crate) base: Option<String>,
    #[arg(skip)]
    pub(crate) changed_paths: Option<Vec<String>>,
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
    pub(crate) history: bool,
    pub(crate) artifact: Option<CiLaneArtifact>,
    pub(crate) queue: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) runner: Option<String>,
    /// The enabled pool the lane runs on.
    pub(crate) os: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct Dependent {
    pub(crate) lane: String,
    pub(crate) timeout: u32,
    pub(crate) history: bool,
    pub(crate) needs: Vec<String>,
    pub(crate) download: Vec<String>,
    pub(crate) artifact: Option<CiLaneArtifact>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) runner: Option<String>,
    pub(crate) os: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct Selection {
    pub(crate) matrix: Vec<Entry>,
    pub(crate) dependent: Vec<Dependent>,
}

/// The kinds this fleet runs the lane in under the profile: its own list
/// when the providers split the catalog, both lists for the provider that
/// runs it alone, and none for the other.
fn membership(lane: &CiLaneConfig, fleet: Fleet, profile: Profile) -> BTreeSet<&str> {
    let (own, other) = match fleet {
        Fleet::Github => (&lane.kinds_github, &lane.kinds_gitlab),
        Fleet::Gitlab => (&lane.kinds_gitlab, &lane.kinds_github),
    };
    let alone = match fleet {
        Fleet::Github => Profile::GithubOnly,
        Fleet::Gitlab => Profile::GitlabOnly,
    };
    match profile {
        Profile::Split => own.iter().map(String::as_str).collect(),
        profile if profile == alone => own.iter().chain(other).map(String::as_str).collect(),
        _ => BTreeSet::new(),
    }
}

/// The enabled pool a lane runs on: the first system the provider lists a
/// pool for that the lane runs on. None means the provider has no pool for it.
fn pool<'a>(lane: &CiLaneConfig, pools: &'a [String]) -> Option<&'a str> {
    pools
        .iter()
        .map(String::as_str)
        .find(|pool| lane.os.iter().any(|os| os == pool))
}

/// A lane with the asked-for role that this pipeline kind schedules, or that
/// `--only` named directly. `--only all` stands for a full run and leaves out
/// a lane only narrowing pipelines schedule: its share of a suite is one the
/// full lanes run whole. A lane with no membership belongs to a dedicated
/// workflow and never enters this fleet's fan-out, even by name.
fn is_asked_for(lane: &CiLaneConfig, name: &str, kind: &str, args: &LanesArgs) -> Result<bool> {
    if lane.role != args.role || pool(lane, &args.pools).is_none() {
        return Ok(false);
    }
    let membership = membership(lane, args.fleet, args.profile);
    if membership.is_empty() {
        return Ok(false);
    }
    if !args.only.is_empty() {
        return Ok(args
            .only
            .iter()
            .any(|only| only == name || (only == "all" && a_full_run_schedules(&membership))));
    }
    if !membership.contains(kind) {
        return Ok(false);
    }
    if args.kind.is_full() {
        return Ok(true);
    }
    args.changed_paths
        .as_deref()
        .map_or(Ok(true), |changed| lane.scheduled_by(name, changed))
}

/// Whether some full pipeline kind schedules a lane with this membership.
fn a_full_run_schedules(membership: &BTreeSet<&str>) -> bool {
    PipelineKind::value_variants()
        .iter()
        .filter(|kind| kind.is_full())
        .any(|kind| membership.contains(kind.name()))
}

pub(crate) fn render(
    lanes: &BTreeMap<String, CiLaneConfig>,
    args: &LanesArgs,
) -> Result<Selection> {
    if !consts::LANE_ROLES.contains(&args.role.as_str()) {
        bail!(
            "`{}` is not a CI lane role; this repository has {}",
            args.role,
            consts::LANE_ROLES.join(", ")
        );
    }
    for name in args.only.iter().filter(|name| name.as_str() != "all") {
        if !lanes.contains_key(name) {
            bail!(
                "`{name}` is not a CI lane; this repository has {}",
                lanes.keys().cloned().collect::<Vec<_>>().join(", ")
            );
        }
    }
    let kind = args.kind.name();

    let mut matrix = Vec::new();
    for (name, lane) in lanes {
        if !lane.needs.is_empty() || !is_asked_for(lane, name, kind, args)? {
            continue;
        }
        if let Some(os) = pool(lane, &args.pools) {
            matrix.push(Entry {
                lane: name.clone(),
                timeout: lane.timeout_minutes,
                history: lane.history,
                artifact: lane.artifact.clone(),
                queue: lane.queue.clone(),
                runner: github_runner(lane, args.kind),
                os: os.to_owned(),
            });
        }
    }

    // A lane with `needs` never earns its own place through `--role`/`--kind`
    // or `--only`: it rides in only once a lane it reads from already made the
    // matrix above. Asking for one producer therefore brings its consumer with
    // it, and asking for an unrelated lane does not. That need-presence test is
    // necessary but not sufficient: by kind (empty `--only`), the consumer
    // still has to answer for this fleet's kind the same way a matrix lane
    // would, or a producer this fleet never runs would drag in a consumer it
    // never asked for either. By name (`--only` naming the producer), the
    // consumer's own membership does not gate it, because that is the whole
    // point of asking for one lane by name. An empty membership still
    // excludes the consumer from that fleet.
    let present: BTreeSet<&str> = matrix.iter().map(|entry| entry.lane.as_str()).collect();
    let dependent: Vec<Dependent> = lanes
        .iter()
        .filter(|(_, lane)| lane.role == args.role && !lane.needs.is_empty())
        .filter(|(_, lane)| {
            lane.needs
                .iter()
                .any(|need| present.contains(need.as_str()))
        })
        .filter(|(_, lane)| {
            let membership = membership(lane, args.fleet, args.profile);
            !membership.is_empty() && (!args.only.is_empty() || membership.contains(kind))
        })
        .filter_map(|(name, lane)| pool(lane, &args.pools).map(|os| (name, lane, os)))
        .map(|(name, lane, os)| Dependent {
            lane: name.clone(),
            timeout: lane.timeout_minutes,
            history: lane.history,
            needs: lane.needs.clone(),
            download: matrix
                .iter()
                .filter(|producer| lane.needs.contains(&producer.lane))
                .filter_map(|producer| producer.artifact.as_ref())
                .map(|artifact| artifact.name.clone())
                .collect(),
            artifact: lane.artifact.clone(),
            runner: github_runner(lane, args.kind),
            os: os.to_owned(),
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
        .filter(|name| *name != "all")
        .filter(|name| !landed.contains(name))
        .filter(|name| lanes[*name].role == args.role)
        .filter(|name| !membership(&lanes[*name], args.fleet, args.profile).is_empty())
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

fn github_runner(lane: &CiLaneConfig, kind: PipelineKind) -> Option<String> {
    if kind == PipelineKind::Main {
        lane.github_runner_main
            .clone()
            .or_else(|| lane.github_runner.clone())
    } else {
        lane.github_runner.clone()
    }
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

fn changed_paths(root: &Path, base: &str) -> Option<Vec<String>> {
    let paths = Command::new("git")
        .current_dir(root)
        .args([
            "diff",
            "--name-only",
            "--no-renames",
            "-z",
            "--merge-base",
            base,
            "HEAD",
            "--",
        ])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|output| {
            output
                .split('\0')
                .filter(|path| !path.is_empty())
                .map(str::to_owned)
                .collect::<Vec<_>>()
        });
    if paths.is_none() {
        warn!(base, "comparison failed; selecting every path-gated lane");
    }
    paths
}

pub(crate) fn run(args: &LanesArgs, ctx: &Ctx) -> Result<()> {
    let ext = KitharaExt::from_ctx(ctx)?;
    ext.ci.validate()?;
    let mut args = args.clone();
    args.changed_paths = args
        .base
        .as_deref()
        .and_then(|base| changed_paths(&ctx.root, base));
    let selection = render(&ext.ci.lanes, &args)?;
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
            kinds_gitlab: kinds.iter().map(|kind| (*kind).to_owned()).collect(),
            kinds_github: kinds.iter().map(|kind| (*kind).to_owned()).collect(),
            needs: needs.iter().map(|need| (*need).to_owned()).collect(),
            timeout_minutes: 30,
            cache_group: "linux".to_owned(),
            os: vec!["linux".to_owned()],
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
            profile: Profile::Split,
            pools: vec!["linux".to_owned()],
            field: None,
            base: None,
            changed_paths: None,
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
    fn main_uses_the_lane_protected_runner() {
        let mut lane = lane("gate", &["branch", "main"], &[]);
        lane.github_runner = Some("kithara-review".to_owned());
        lane.github_runner_main = Some("kithara-trusted-snapshot".to_owned());
        let catalog = BTreeMap::from([("linux-test".to_owned(), lane)]);

        let branch = render(&catalog, &args("gate", PipelineKind::Branch, &[])).unwrap();
        let main = render(&catalog, &args("gate", PipelineKind::Main, &[])).unwrap();

        assert_eq!(branch.matrix[0].runner.as_deref(), Some("kithara-review"));
        assert_eq!(
            main.matrix[0].runner.as_deref(),
            Some("kithara-trusted-snapshot")
        );
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

    /// The workflow picks a report's upload from the matrix row alone, so the
    /// row names the report and no checkout path.
    #[test]
    fn a_report_artifact_reaches_the_matrix_without_a_path() {
        let mut lanes = catalog();
        lanes.get_mut("linux-lint").unwrap().artifact = Some(CiLaneArtifact {
            name: "web-chromium-driver".to_owned(),
            path: None,
            report: Some(crate::config::LaneReport::Chromium),
            when: ArtifactWhen::Failure,
        });

        let selection =
            render(&lanes, &args("gate", PipelineKind::Main, &[])).expect("the gate role renders");

        assert_eq!(
            serde_json::to_value(&selection.matrix[0].artifact).unwrap(),
            serde_json::json!({
                "name": "web-chromium-driver",
                "report": "chromium",
                "when": "failure",
            })
        );
    }

    // Asking for one lane must bring what reads its artifact, and nothing else.
    #[test]
    fn selecting_one_lane_brings_only_its_own_dependents() {
        let mut catalog = catalog();
        catalog.get_mut("deep-stress").unwrap().artifact = Some(CiLaneArtifact {
            name: "stress-evidence".to_owned(),
            path: Some("target/stress".to_owned()),
            report: None,
            when: ArtifactWhen::Always,
        });
        let dependent = catalog
            .get_mut("deep-stress-report")
            .expect("the dependent lane is in the catalog");
        dependent.artifact = Some(CiLaneArtifact {
            name: "quality-report".to_owned(),
            path: Some("target/consolidated-quality-report.md".to_owned()),
            report: None,
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
            r#"[{"lane":"deep-stress-report","timeout":30,"history":false,"needs":["deep-stress"],"download":["stress-evidence"],"artifact":{"name":"quality-report","path":"target/consolidated-quality-report.md","when":"failure"},"os":"linux"}]"#
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
    fn dependent_downloads_exclude_unselected_and_unrelated_producers() {
        let mut lanes = catalog();
        for (name, artifact) in [
            ("deep-stress", "stress-evidence"),
            ("deep-miri", "miri-junit"),
        ] {
            lanes.get_mut(name).unwrap().artifact = Some(CiLaneArtifact {
                name: artifact.to_owned(),
                path: Some("target/report".to_owned()),
                report: None,
                when: ArtifactWhen::Always,
            });
        }
        let selection = render(&lanes, &args("deep", PipelineKind::Branch, &["all"]))
            .expect("both producers are selected");
        assert_eq!(selection.matrix.len(), 2);
        assert_eq!(selection.dependent[0].download, ["stress-evidence"]);

        lanes
            .get_mut("deep-stress-report")
            .unwrap()
            .needs
            .push("deep-miri".to_owned());
        let selection = render(&lanes, &args("deep", PipelineKind::Nightly, &[]))
            .expect("only the nightly producer is selected");
        assert_eq!(selection.matrix.len(), 1);
        assert_eq!(selection.dependent[0].download, ["stress-evidence"]);

        lanes.get_mut("deep-stress").unwrap().artifact = None;
        let selection = render(&lanes, &args("deep", PipelineKind::Nightly, &[]))
            .expect("a producer without an artifact still schedules its dependent");
        assert!(selection.dependent[0].download.is_empty());
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
    fn all_selects_every_reachable_lane_a_full_kind_schedules() {
        let mut lanes = catalog();
        lanes.insert("linux-extra".to_owned(), lane("gate", &["weekly"], &[]));
        lanes.insert("deep-extra".to_owned(), lane("deep", &["weekly"], &[]));

        let selection = render(&lanes, &args("gate", PipelineKind::Branch, &["all"]))
            .expect("every gate lane a full kind schedules renders");
        let names: Vec<&str> = selection
            .matrix
            .iter()
            .map(|entry| entry.lane.as_str())
            .collect();
        assert_eq!(names, ["linux-extra", "linux-lint"]);
    }

    #[test]
    fn an_only_this_role_owns_that_this_fleet_cannot_reach_is_refused_rather_than_empty() {
        // A name bypasses its lane's kinds, so what is left to refuse is the
        // machine: asking this fleet for a macOS lane must fail rather than
        // return an empty selection a caller reads as success.
        let mut lanes = catalog();
        let mut elsewhere = lane("deep", &["nightly"], &[]);
        elsewhere.os = vec!["macos".to_owned()];
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
    fn an_only_with_no_membership_stays_in_its_dedicated_workflow() {
        let mut lanes = catalog();
        lanes.insert("deep-ui".to_owned(), lane("deep", &[], &[]));

        let selection = render(&lanes, &args("deep", PipelineKind::Nightly, &["deep-ui"]))
            .expect("a dedicated lane is not part of the generic fan-out");

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
        deny.kinds_gitlab = vec!["weekly".to_owned()];
        deny.kinds_github = vec!["main".to_owned()];

        let github = render(&lanes, &args("gate", PipelineKind::Main, &[]))
            .expect("the GitHub fleet renders");
        assert_eq!(github.matrix.len(), 1);

        let mut gitlab = args("gate", PipelineKind::Main, &[]);
        gitlab.fleet = Fleet::Gitlab;
        let gitlab = render(&lanes, &gitlab).expect("the GitLab fleet renders");
        assert!(gitlab.matrix.is_empty());
    }

    #[test]
    fn portable_lanes_reach_linux_without_scheduling_dedicated_device_work() {
        let mut lanes = catalog();
        let mut portable = lane("gate", &["main"], &[]);
        portable.os = vec!["macos".to_owned(), "linux".to_owned()];
        lanes.insert("deps-deny".to_owned(), portable);
        let mut device = lane("gate", &["main"], &[]);
        device.os = vec!["macos".to_owned(), "linux".to_owned()];
        device.kinds_github = Vec::new();
        lanes.insert("android-test".to_owned(), device);
        let github = render(&lanes, &args("gate", PipelineKind::Main, &[])).unwrap();
        let names: Vec<&str> = github
            .matrix
            .iter()
            .map(|entry| entry.lane.as_str())
            .collect();
        assert_eq!(names, ["deps-deny", "linux-lint"]);
    }

    #[test]
    fn a_provider_without_a_pool_for_the_system_does_not_schedule_the_lane() {
        let mut lanes = catalog();
        let mut elsewhere = lane("gate", &["main"], &[]);
        elsewhere.os = vec!["macos".to_owned()];
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
        gitlab.pools = vec!["linux".to_owned(), "macos".to_owned()];
        let gitlab = render(&lanes, &gitlab).expect("the GitLab fleet renders");
        assert!(
            gitlab
                .matrix
                .iter()
                .any(|entry| entry.lane == "deep-rtsan" && entry.os == "macos"),
            "GitLab has the machine and still schedules the lane"
        );
    }

    #[test]
    fn a_github_macos_pool_runs_the_lane_its_own_membership_names() {
        let mut lanes = catalog();
        let mut apple = lane("gate", &["main"], &[]);
        apple.os = vec!["macos".to_owned()];
        lanes.insert("apple-lint".to_owned(), apple);
        let mut asked = args("gate", PipelineKind::Main, &[]);
        asked.pools = vec!["linux".to_owned(), "macos".to_owned()];

        let github = render(&lanes, &asked).expect("the GitHub fleet renders");
        let placed: Vec<(&str, &str)> = github
            .matrix
            .iter()
            .map(|entry| (entry.lane.as_str(), entry.os.as_str()))
            .collect();
        assert_eq!(placed, [("apple-lint", "macos"), ("linux-lint", "linux")]);
    }

    /// Split spreads the catalog by each provider's own list; a provider that
    /// runs alone takes every lane either list names, and the other renders
    /// nothing, so a switch of profile never drops or doubles a lane.
    #[test]
    fn a_profile_moves_every_lane_to_one_provider_without_dropping_any() -> Result<()> {
        let mut gitlab_only = lane("gate", &[], &[]);
        gitlab_only.kinds_gitlab = vec!["main".to_owned()];
        let mut github_only = lane("gate", &[], &[]);
        github_only.kinds_github = vec!["main".to_owned()];
        let catalog = BTreeMap::from([
            ("on-gitlab".to_owned(), gitlab_only),
            ("on-github".to_owned(), github_only),
        ]);
        let names = |fleet: Fleet, profile: Profile| -> Result<Vec<String>> {
            let mut asked = args("gate", PipelineKind::Main, &[]);
            asked.fleet = fleet;
            asked.profile = profile;
            Ok(render(&catalog, &asked)?
                .matrix
                .into_iter()
                .map(|entry| entry.lane)
                .collect())
        };
        let both = ["on-github", "on-gitlab"];
        assert_eq!(names(Fleet::Github, Profile::Split)?, ["on-github"]);
        assert_eq!(names(Fleet::Gitlab, Profile::Split)?, ["on-gitlab"]);
        assert_eq!(names(Fleet::Github, Profile::GithubOnly)?, both);
        assert!(names(Fleet::Gitlab, Profile::GithubOnly)?.is_empty());
        assert_eq!(names(Fleet::Gitlab, Profile::GitlabOnly)?, both);
        assert!(names(Fleet::Github, Profile::GitlabOnly)?.is_empty());
        Ok(())
    }

    /// `--only all` stands for a full run. A lane only narrowing pipelines
    /// schedule gives a branch push its share of suites the full lanes run
    /// whole, so a full run leaves it out, whatever kind asks; naming it still
    /// runs it.
    #[test]
    fn all_leaves_out_a_lane_only_a_narrowing_pipeline_schedules() {
        let mut lanes = catalog();
        lanes.insert("linux-branch".to_owned(), lane("gate", &["branch"], &[]));

        for kind in [PipelineKind::Main, PipelineKind::Branch] {
            let full = render(&lanes, &args("gate", kind, &["all"])).expect("a full run renders");
            let names: Vec<&str> = full
                .matrix
                .iter()
                .map(|entry| entry.lane.as_str())
                .collect();
            assert_eq!(names, ["linux-lint"]);
        }
        let named = render(&lanes, &args("gate", PipelineKind::Main, &["linux-branch"]))
            .expect("a lane renders by name");
        let names: Vec<&str> = named
            .matrix
            .iter()
            .map(|entry| entry.lane.as_str())
            .collect();
        assert_eq!(names, ["linux-branch"]);
    }

    /// A `main` push renders these four roles with `--only all` and runs each
    /// `--lane` a GitHub lane names exactly once: a test lane two of its lanes
    /// run is built twice for nothing, and one only a narrowing pipeline runs
    /// is never judged whole before a release.
    #[test]
    fn a_main_push_runs_every_test_lane_a_github_lane_names_once() {
        let root = crate::testing::workspace::root();
        let ext = KitharaExt::load(&root).expect("the project config parses");
        let mut carriers: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for role in ["gate", "deep", "platforms", "quality"] {
            let selection = render(&ext.ci.lanes, &args(role, PipelineKind::Main, &["all"]))
                .expect("a main push renders every role");
            let selected = selection
                .matrix
                .iter()
                .map(|entry| entry.lane.as_str())
                .chain(selection.dependent.iter().map(|entry| entry.lane.as_str()));
            for (name, lane) in selected.filter_map(|name| ext.ci.lanes.get_key_value(name)) {
                for step in &lane.steps {
                    let step_args = step
                        .args_by_kind
                        .get(PipelineKind::Main.name())
                        .unwrap_or(&step.args);
                    for test_lane in step_args
                        .iter()
                        .filter_map(|arg| arg.strip_prefix("--lane="))
                    {
                        carriers.entry(test_lane).or_default().push(name.as_str());
                    }
                }
            }
        }
        let repeated: Vec<String> = carriers
            .iter()
            .filter(|(_, lanes)| lanes.len() > 1)
            .map(|(test_lane, lanes)| format!("{test_lane}: {}", lanes.join(", ")))
            .collect();
        assert!(
            repeated.is_empty(),
            "main runs a test lane twice: {repeated:?}"
        );
        let named: BTreeSet<&str> = ext
            .ci
            .lanes
            .values()
            .filter(|lane| {
                lane.os.iter().any(|os| os == "linux")
                    && !membership(lane, Fleet::Github, Profile::Split).is_empty()
            })
            .flat_map(|lane| &lane.steps)
            .flat_map(|step| step.args.iter().chain(step.args_by_kind.values().flatten()))
            .filter_map(|arg| arg.strip_prefix("--lane="))
            .collect();
        let unrun: Vec<&str> = named
            .into_iter()
            .filter(|test_lane| !carriers.contains_key(test_lane))
            .collect();
        assert!(unrun.is_empty(), "main runs no lane that carries {unrun:?}");
    }

    #[test]
    fn configured_feature_parts_are_github_gates_and_gitlab_keeps_full() -> Result<()> {
        let ctx = Ctx::load()?;
        let lanes = KitharaExt::from_ctx(&ctx)?.ci.lanes;
        let feature_lanes = lanes
            .iter()
            .filter(|(_, lane)| {
                lane.steps
                    .iter()
                    .any(|step| step.args.iter().any(|arg| arg.starts_with("hack")))
            })
            .collect::<BTreeMap<_, _>>();
        let parts = feature_lanes
            .iter()
            .filter(|(_, lane)| lane.kinds_gitlab.is_empty())
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(parts.len(), 3);
        let full = feature_lanes
            .iter()
            .filter(|(_, lane)| !lane.kinds_gitlab.is_empty())
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(full.len(), 1);
        for kind in [PipelineKind::Main, PipelineKind::Nightly] {
            for only in [Vec::new(), vec!["all"]] {
                let selected = render(&lanes, &args("gate", kind, &only))?;
                let features = selected
                    .matrix
                    .iter()
                    .filter(|entry| feature_lanes.keys().any(|name| name.as_str() == entry.lane))
                    .map(|entry| entry.lane.as_str())
                    .collect::<Vec<_>>();
                assert_eq!(features, parts);
            }
            for name in &parts {
                let lane = &lanes[*name];
                assert_eq!(lane.timeout_minutes, 60);
                assert!(lane.needs.is_empty());
            }
        }
        let selected = render(&lanes, &args("gate", PipelineKind::Nightly, &parts))?;
        assert_eq!(
            selected
                .matrix
                .iter()
                .map(|entry| entry.lane.as_str())
                .collect::<Vec<_>>(),
            parts,
        );
        let weekly = render(&lanes, &args("gate", PipelineKind::Weekly, &[]))?;
        assert!(
            weekly
                .matrix
                .iter()
                .all(|entry| !feature_lanes.keys().any(|name| name.as_str() == entry.lane)),
        );
        let weekly = render(&lanes, &args("gate", PipelineKind::Weekly, &["all"]))?;
        let features = weekly
            .matrix
            .iter()
            .filter(|entry| feature_lanes.keys().any(|name| name.as_str() == entry.lane))
            .map(|entry| entry.lane.as_str())
            .collect::<Vec<_>>();
        assert_eq!(features, parts);
        for only in [Vec::new(), vec!["all"]] {
            let mut weekly = args("gate", PipelineKind::Weekly, &only);
            weekly.fleet = Fleet::Gitlab;
            let selected = render(&lanes, &weekly)?;
            let features = selected
                .matrix
                .iter()
                .filter(|entry| feature_lanes.keys().any(|name| name.as_str() == entry.lane))
                .map(|entry| entry.lane.as_str())
                .collect::<Vec<_>>();
            assert_eq!(features, full);
        }
        Ok(())
    }

    #[test]
    fn github_membership_is_read_from_its_own_list_alone() -> Result<()> {
        for (own, main_default, weekly_default, full, named) in [
            (vec!["weekly".to_owned()], 0, 1, 1, true),
            (Vec::new(), 0, 0, 0, false),
            (vec!["main".to_owned()], 1, 0, 1, true),
        ] {
            let mut configured = lane("gate", &["weekly"], &[]);
            configured.kinds_github = own;
            let catalog = BTreeMap::from([("feature-check".to_owned(), configured)]);
            for (kind, default) in [
                (PipelineKind::Main, main_default),
                (PipelineKind::Weekly, weekly_default),
            ] {
                let selected = render(&catalog, &args("gate", kind, &[]))?;
                assert_eq!(selected.matrix.len(), default);
                let selected = render(&catalog, &args("gate", kind, &["all"]))?;
                assert_eq!(selected.matrix.len(), full);
                let selected = render(&catalog, &args("gate", kind, &["feature-check"]));
                if named {
                    assert_eq!(selected?.matrix.len(), 1);
                } else {
                    let selected = selected?;
                    assert!(selected.matrix.is_empty());
                    assert!(selected.dependent.is_empty());
                }
                let mut gitlab = args("gate", kind, &[]);
                gitlab.fleet = Fleet::Gitlab;
                let default = usize::from(kind == PipelineKind::Weekly);
                assert_eq!(render(&catalog, &gitlab)?.matrix.len(), default);
                for only in [vec!["all"], vec!["feature-check"]] {
                    gitlab.only = only.into_iter().map(str::to_owned).collect();
                    assert_eq!(render(&catalog, &gitlab)?.matrix.len(), 1);
                }
            }
        }
        Ok(())
    }

    #[test]
    fn an_empty_github_list_cannot_follow_a_selected_producer() -> Result<()> {
        for (own, main_default, weekly_default, requested) in [
            (vec!["weekly".to_owned()], 0, 1, 1),
            (Vec::new(), 0, 0, 0),
            (vec!["main".to_owned()], 1, 0, 1),
        ] {
            let mut dependent = lane("deep", &["weekly"], &["producer"]);
            dependent.kinds_github = own;
            let catalog = BTreeMap::from([
                (
                    "producer".to_owned(),
                    lane("deep", &["main", "weekly"], &[]),
                ),
                ("report".to_owned(), dependent),
            ]);
            for (kind, default) in [
                (PipelineKind::Main, main_default),
                (PipelineKind::Weekly, weekly_default),
            ] {
                let selected = render(&catalog, &args("deep", kind, &[]))?;
                assert_eq!(selected.dependent.len(), default);
                for only in [&["all"][..], &["producer"][..]] {
                    let selected = render(&catalog, &args("deep", kind, only))?;
                    assert_eq!(selected.dependent.len(), requested);
                }
                let mut gitlab = args("deep", kind, &[]);
                gitlab.fleet = Fleet::Gitlab;
                let default = usize::from(kind == PipelineKind::Weekly);
                assert_eq!(render(&catalog, &gitlab)?.dependent.len(), default);
                for only in [vec!["all"], vec!["producer"]] {
                    gitlab.only = only.into_iter().map(str::to_owned).collect();
                    assert_eq!(render(&catalog, &gitlab)?.dependent.len(), 1);
                }
            }
        }
        Ok(())
    }

    #[test]
    fn dependency_inputs_schedule_advisories_on_review_pipelines() -> Result<()> {
        let root = crate::testing::workspace::root();
        let ext = KitharaExt::load(&root)?;
        for (fleet, kind) in [
            (Fleet::Gitlab, PipelineKind::Branch),
            (Fleet::Gitlab, PipelineKind::Quarantine),
        ] {
            for (changed, expected) in [
                (&["crates/kithara/src/lib.rs"][..], &["deps-deny"][..]),
                (&["Cargo.lock"][..], &["deps-advisories", "deps-deny"][..]),
                (&["Cargo.toml"][..], &["deps-advisories", "deps-deny"][..]),
                (
                    &["crates/kithara/Cargo.toml"][..],
                    &["deps-advisories", "deps-deny"][..],
                ),
                (&["deny.toml"][..], &["deps-advisories", "deps-deny"][..]),
                (
                    &[".cargo/config.toml"][..],
                    &["deps-advisories", "deps-deny"][..],
                ),
            ] {
                let mut args = args("gate", kind, &[]);
                args.fleet = fleet;
                args.changed_paths = Some(changed.iter().map(|path| (*path).to_owned()).collect());
                let selected = render(&ext.ci.lanes, &args)?;
                let dependency_lanes = selected
                    .matrix
                    .iter()
                    .map(|entry| entry.lane.as_str())
                    .filter(|name| ["deps-advisories", "deps-deny"].contains(name))
                    .collect::<Vec<_>>();
                assert_eq!(
                    dependency_lanes,
                    expected,
                    "{fleet:?}/{}: {changed:?}",
                    kind.name()
                );
            }
        }

        for (fleet, kind) in [
            (Fleet::Gitlab, PipelineKind::Main),
            (Fleet::Gitlab, PipelineKind::Weekly),
        ] {
            let mut args = args("gate", kind, &[]);
            args.fleet = fleet;
            args.changed_paths = Some(vec!["crates/kithara/src/lib.rs".to_owned()]);
            let selected = render(&ext.ci.lanes, &args)?;
            for name in ["deps-advisories", "deps-deny"] {
                assert!(
                    selected.matrix.iter().any(|entry| entry.lane == name),
                    "{fleet:?}/{} must always schedule {name}",
                    kind.name()
                );
            }
        }
        Ok(())
    }

    #[test]
    fn an_unknown_comparison_selects_path_gated_lanes() -> Result<()> {
        let root = crate::testing::workspace::root();
        assert!(changed_paths(&root, "HEAD").is_some());
        assert_eq!(changed_paths(&root, "refs/kithara/missing"), None);

        let ext = KitharaExt::load(&root)?;
        for (fleet, kind) in [
            (Fleet::Gitlab, PipelineKind::Branch),
            (Fleet::Gitlab, PipelineKind::Quarantine),
        ] {
            let mut args = args("gate", kind, &[]);
            args.fleet = fleet;
            args.changed_paths = None;
            let selected = render(&ext.ci.lanes, &args)?;
            assert!(
                selected
                    .matrix
                    .iter()
                    .any(|entry| entry.lane == "deps-advisories"),
                "{fleet:?}/{} must fail open when the comparison is unknown",
                kind.name()
            );
        }
        Ok(())
    }
}
