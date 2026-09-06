use std::process::Command;

use anyhow::{Result, bail};
use clap::Args;

use crate::{Ctx, common::project::FeatureInvariant};

struct Consts;
impl Consts {
    const DEPTH: &'static str = "2";
}

#[derive(Debug, Args)]
pub struct PowersetArgs {
    /// Skip dev-dependencies. The health run leaves them in, because a crate
    /// whose tests need a feature its library does not is still a crate whose
    /// feature set is wrong.
    #[arg(long)]
    pub no_dev_deps: bool,
}

pub(crate) fn run(args: &PowersetArgs, ctx: &Ctx) -> Result<()> {
    for command in plan(ctx, args.no_dev_deps)? {
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

/// Every `cargo hack` invocation this workspace needs, in order.
///
/// One pass covers the workspace. Crates that declare a backend invariant are
/// held out of it and checked on their own with that invariant applied, because
/// `--at-least-one-of` silently drops combinations from a crate that has none
/// of the named features — applied workspace-wide it would quietly narrow the
/// coverage of every other crate.
fn plan(ctx: &Ctx, no_dev_deps: bool) -> Result<Vec<Vec<String>>> {
    let health = &ctx.config.health;
    let declared = declaring_crates(ctx)?;

    let base = |extra: &[&str]| {
        let mut args = vec![
            "hack".to_owned(),
            "check".to_owned(),
            "--feature-powerset".to_owned(),
            "--depth".to_owned(),
            Consts::DEPTH.to_owned(),
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
    Ok(plan)
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
    use crate::common::project::FeatureInvariant;

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
}
