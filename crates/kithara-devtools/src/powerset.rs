use std::process::Command;

use anyhow::{Result, bail};
use clap::{Args, ValueEnum};

use crate::{Ctx, common::project::FeatureInvariant, consts};

#[derive(Debug, Args)]
pub struct PowersetArgs {
    /// Skip dev-dependencies. The health run leaves them in, because a crate
    /// whose tests need a feature its library does not is still a crate whose
    /// feature set is wrong.
    #[arg(long)]
    pub no_dev_deps: bool,
    /// Select one complete part of the canonical feature-check plan.
    #[arg(long, value_enum, default_value_t = Part::Full)]
    part: Part,
    /// Forward the native cargo-hack M/N partition to each invocation.
    #[arg(long)]
    partition: Option<String>,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum Part {
    Full,
    Workspace,
    Invariants,
}

pub(crate) fn run(args: &PowersetArgs, ctx: &Ctx) -> Result<()> {
    for command in plan(ctx, args.no_dev_deps, args.part, args.partition.as_deref())? {
        let status = Command::new("cargo")
            .args(&command)
            .status()
            .map_err(|error| anyhow::anyhow!("running cargo {}: {error}", command.join(" ")))?;
        if !status.success() {
            bail!("cargo {} failed", command.join(" "));
        }
    }
    Ok(())
}

/// The selected complete part of the workspace feature-check plan, in order.
///
/// One pass covers the workspace. Crates that declare a backend invariant are
/// held out of it and checked on their own with that invariant applied, because
/// `--at-least-one-of` silently drops combinations from a crate that has none
/// of the named features — applied workspace-wide it would quietly narrow the
/// coverage of every other crate.
fn plan(
    ctx: &Ctx,
    no_dev_deps: bool,
    part: Part,
    partition: Option<&str>,
) -> Result<Vec<Vec<String>>> {
    let health = &ctx.config.health;
    let declared = declaring_crates(ctx)?;

    let base = |extra: &[&str]| {
        let mut args = vec![
            "hack".to_owned(),
            "check".to_owned(),
            "--feature-powerset".to_owned(),
            "--depth".to_owned(),
            consts::DEPTH.to_owned(),
        ];
        if no_dev_deps {
            args.push("--no-dev-deps".to_owned());
        }
        args.extend(extra.iter().map(|arg| (*arg).to_owned()));
        args
    };

    let mut workspace = base(&["--workspace"]);
    for krate in health
        .feature_powerset_exclude
        .iter()
        .chain(declared.iter().map(|(krate, _)| krate))
    {
        workspace.push("--exclude".to_owned());
        workspace.push(krate.clone());
    }

    let mut plan = vec![workspace];
    for (krate, invariants) in &declared {
        let mut args = base(&["-p", krate]);
        for invariant in invariants {
            args.extend(invariant.args());
        }
        plan.push(args);
    }
    if let Some(partition) = partition {
        for command in &mut plan {
            command.extend(["--partition".to_owned(), partition.to_owned()]);
        }
    }
    Ok(match part {
        Part::Full => plan,
        Part::Workspace => plan.into_iter().take(1).collect(),
        Part::Invariants => plan.into_iter().skip(1).collect(),
    })
}

/// Which crates carry which invariants, read from their manifests.
///
/// A crate declares an invariant by declaring the feature that names it, so the
/// list follows the workspace instead of being repeated beside it.
fn declaring_crates(ctx: &Ctx) -> Result<Vec<(String, Vec<&FeatureInvariant>)>> {
    let excluded = &ctx.config.health.feature_powerset_exclude;
    let mut declaring = Vec::new();
    for package in ctx.metadata()?.workspace_packages() {
        let name = package.name.to_string();
        if excluded.contains(&name) {
            continue;
        }
        let invariants = ctx
            .config
            .health
            .feature_invariants
            .iter()
            .filter(|invariant| package.features.contains_key(&invariant.when_feature))
            .collect::<Vec<_>>();
        if !invariants.is_empty() {
            declaring.push((name, invariants));
        }
    }
    declaring.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(declaring)
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeSet, fs};

    use anyhow::Result;
    use clap::Parser;

    use super::{Part, PowersetArgs, plan};
    use crate::{Ctx, common::project::FeatureInvariant};

    fn invariant(when: &str, groups: &[&[&str]], always: &[&str]) -> FeatureInvariant {
        FeatureInvariant {
            when_feature: when.to_owned(),
            at_least_one_of: groups
                .iter()
                .map(|group| group.iter().map(|name| (*name).to_owned()).collect())
                .collect(),
            always: always.iter().map(|name| (*name).to_owned()).collect(),
            ..FeatureInvariant::default()
        }
    }

    /// `--at-least-one-of` needs two or more names; cargo-hack rejects a
    /// single-name group outright, so a lone feature has to be forced on
    /// instead of chosen between.
    #[test]
    fn a_group_of_one_is_forced_rather_than_offered() {
        let single = invariant("symphonia", &[], &["symphonia"]);
        assert_eq!(single.args(), vec!["--features", "symphonia"]);

        let pair = invariant("client-reqwest", &[&["client-reqwest", "client-wreq"]], &[]);
        assert_eq!(
            pair.args(),
            vec!["--at-least-one-of", "client-reqwest,client-wreq"]
        );
    }

    #[test]
    fn a_derived_feature_is_excluded_while_the_models_it_names_exclude_each_other() {
        let beat = FeatureInvariant {
            when_feature: "embed-model".to_owned(),
            never: vec!["embed-model".to_owned()],
            mutually_exclusive: vec![vec![
                "embed-small-model".to_owned(),
                "embed-full-model".to_owned(),
                "embed-full-int8-model".to_owned(),
            ]],
            ..FeatureInvariant::default()
        };
        assert_eq!(
            beat.args(),
            vec![
                "--exclude-features",
                "embed-model",
                "--mutually-exclusive-features",
                "embed-small-model,embed-full-model,embed-full-int8-model",
            ]
        );
    }

    #[test]
    fn a_crate_carrying_two_invariants_gets_both_in_one_invocation() {
        let both = [
            invariant("client-reqwest", &[&["client-reqwest", "client-wreq"]], &[]),
            invariant("symphonia", &[], &["symphonia"]),
        ];
        let args = both
            .iter()
            .flat_map(FeatureInvariant::args)
            .collect::<Vec<_>>();
        assert!(
            args.contains(&"client-reqwest,client-wreq".to_owned()),
            "{args:?}"
        );
        assert!(args.contains(&"symphonia".to_owned()), "{args:?}");
    }

    fn workspace() -> Result<(tempfile::TempDir, Ctx)> {
        let root = tempfile::tempdir()?;
        fs::write(
            root.path().join("Cargo.toml"),
            r#"[workspace]
members = ["plain", "alpha", "dual", "excluded"]
resolver = "2"
"#,
        )?;
        for (name, features) in [
            ("plain", "extra = []\n"),
            (
                "alpha",
                "transport = []\ntransport-other = []\nsecure = []\nsecure-other = []\n",
            ),
            (
                "dual",
                "transport = []\ntransport-other = []\nsecure = []\nsecure-other = []\ncodec = []\n",
            ),
            ("excluded", "transport = []\n"),
        ] {
            let package = root.path().join(name);
            fs::create_dir_all(package.join("src"))?;
            fs::write(package.join("src/lib.rs"), "")?;
            fs::write(
                package.join("Cargo.toml"),
                format!(
                    "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[features]\n{features}"
                ),
            )?;
        }
        fs::create_dir_all(root.path().join(".config"))?;
        fs::write(
            root.path().join(".config/xtask.toml"),
            r#"[health]
feature_powerset_exclude = ["excluded"]

[[health.feature_invariants]]
when_feature = "transport"
at_least_one_of = [
    ["transport", "transport-other"],
    ["secure", "secure-other"],
]

[[health.feature_invariants]]
when_feature = "codec"
always = ["codec"]
"#,
        )?;
        let ctx = Ctx::load_from_manifest(&root.path().join("Cargo.toml"))?;
        Ok((root, ctx))
    }

    #[test]
    fn parts_preserve_every_ordered_check_and_invariant_in_both_dev_modes() -> Result<()> {
        let (_root, ctx) = workspace()?;
        let expected = [
            "hack check --feature-powerset --depth 2 --workspace --exclude excluded --exclude alpha --exclude dual",
            "hack check --feature-powerset --depth 2 -p alpha --at-least-one-of transport,transport-other --at-least-one-of secure,secure-other",
            "hack check --feature-powerset --depth 2 -p dual --at-least-one-of transport,transport-other --at-least-one-of secure,secure-other --features codec",
        ]
        .map(|command| {
            command
                .split_whitespace()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        });
        for no_dev_deps in [false, true] {
            let mut expected = expected.to_vec();
            if no_dev_deps {
                for command in &mut expected {
                    command.insert(5, "--no-dev-deps".to_owned());
                }
            }
            let full = plan(&ctx, no_dev_deps, Part::Full, None)?;
            let workspace = plan(&ctx, no_dev_deps, Part::Workspace, None)?;
            let invariants = plan(&ctx, no_dev_deps, Part::Invariants, None)?;
            assert_eq!(full, expected);
            assert_eq!(workspace, full[..1]);
            assert_eq!(invariants, full[1..]);
            assert_eq!(
                workspace.iter().chain(&invariants).collect::<Vec<_>>(),
                full.iter().collect::<Vec<_>>(),
            );
            let workspace = workspace.iter().collect::<BTreeSet<_>>();
            let invariants = invariants.iter().collect::<BTreeSet<_>>();
            assert!(workspace.is_disjoint(&invariants));
            assert_eq!(workspace.len() + invariants.len(), full.len());
        }
        Ok(())
    }

    #[test]
    fn forwarding_consumers_inherit_the_model_policy_without_internal_markers() -> Result<()> {
        let manifest = crate::testing::package().join("../../Cargo.toml");
        let ctx = Ctx::load_from_manifest(&manifest)?;
        for no_dev_deps in [false, true] {
            let commands = plan(&ctx, no_dev_deps, Part::Invariants, None)?;
            for package in ["kithara-analyze", "kithara-analysis", "kithara"] {
                let command = commands
                    .iter()
                    .find(|args| args.windows(2).any(|pair| pair == ["-p", package]))
                    .expect("model consumer remains in the invariant checks");
                for required in [
                    ["--exclude-features", "beat-nn-full-int8"],
                    [
                        "--mutually-exclusive-features",
                        "beat-nn-small,beat-nn-full",
                    ],
                ] {
                    assert!(
                        command.windows(2).any(|pair| {
                            pair[0] == required[0]
                                && required[1]
                                    .split(',')
                                    .all(|feature| pair[1].split(',').any(|name| name == feature))
                        }),
                        "{package}: {command:?} lacks {required:?}",
                    );
                }
            }
        }
        Ok(())
    }

    #[test]
    fn native_partition_preserves_every_invocation_and_its_invariants() -> Result<()> {
        let (_root, ctx) = workspace()?;
        for part in [Part::Full, Part::Workspace, Part::Invariants] {
            let full = plan(&ctx, true, part, None)?;
            for partition in ["1/2", "2/2"] {
                let selected = plan(&ctx, true, part, Some(partition))?;
                assert_eq!(selected.len(), full.len());
                for (command, original) in selected.iter().zip(&full) {
                    assert_eq!(&command[..command.len() - 2], original);
                    assert_eq!(&command[command.len() - 2..], ["--partition", partition]);
                }
            }
        }
        Ok(())
    }

    #[test]
    fn omitted_part_preserves_the_complete_health_cli_contract() -> Result<()> {
        #[derive(Parser)]
        struct Cli {
            #[command(flatten)]
            powerset: PowersetArgs,
        }

        let health = Cli::try_parse_from(["powerset"])?;
        assert!(matches!(health.powerset.part, Part::Full));
        assert!(!health.powerset.no_dev_deps);
        assert!(health.powerset.partition.is_none());
        let dependency = Cli::try_parse_from(["powerset", "--no-dev-deps"])?;
        assert!(matches!(dependency.powerset.part, Part::Full));
        assert!(dependency.powerset.no_dev_deps);
        let partitioned =
            Cli::try_parse_from(["powerset", "--part", "invariants", "--partition", "1/2"])?;
        assert!(matches!(partitioned.powerset.part, Part::Invariants));
        assert_eq!(partitioned.powerset.partition.as_deref(), Some("1/2"));
        Ok(())
    }
}
