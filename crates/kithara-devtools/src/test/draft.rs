use std::{
    collections::{BTreeSet, HashSet},
    path::Path,
};

use anyhow::{Context, Result, bail};
use guppy::{MetadataCommand, graph::PackageGraph};
use nextest_filtering::{Filterset, FiltersetKind, KnownGroups, ParseContext};
use serde::Deserialize;

use crate::common::project::{TestCargoOptions, TestCommandConfig};

/// The environment variable a draft pipeline hands its pull request's test
/// block in, as the TOML between the fences.
pub const PR_TESTS_ENV: &str = "KITHARA_PR_TESTS";

/// What a pull request's `kithara-tests` block asks a draft to run beside the
/// required packages.
#[derive(Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
struct PrTests {
    /// Test packages to run whole.
    packages: Vec<String>,
    /// A nextest filterset whose tests run too.
    filter: Option<String>,
}

/// The tests a draft runs: the required packages, and what its pull request
/// names. Each lane runs the part of it the lane holds.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct DraftScope {
    packages: BTreeSet<String>,
    filter: Option<String>,
    members: BTreeSet<String>,
}

impl DraftScope {
    /// The draft scope of the workspace at `root`, with the pull request's
    /// block `block` when it carries one.
    pub(crate) fn load(test: &TestCommandConfig, root: &Path, block: Option<&str>) -> Result<Self> {
        let (graph, named) = read_scope(root, block)?;
        Self::of(test, &graph, named)
    }

    fn of(test: &TestCommandConfig, graph: &PackageGraph, named: PrTests) -> Result<Self> {
        let members: BTreeSet<String> = graph
            .workspace()
            .iter()
            .map(|package| package.name().to_owned())
            .collect();
        validate_block(graph, &members, &named)?;
        if test.required.packages.is_empty() {
            bail!("test.required.packages in .config/xtask.toml names no package");
        }
        for package in &test.required.packages {
            if !members.contains(package) {
                bail!("test.required.packages names `{package}`, which is not a workspace package");
            }
        }
        let packages = test
            .required
            .packages
            .iter()
            .chain(&named.packages)
            .cloned()
            .collect();
        Ok(Self {
            packages,
            filter: named.filter,
            members,
        })
    }

    /// Whether a lane that builds `cargo` holds any of the scope. A block
    /// filterset may select tests of any package, so a lane always holds it.
    pub(crate) fn holds(&self, cargo: &TestCargoOptions) -> bool {
        self.filter.is_some()
            || self.packages.iter().any(|package| {
                if cargo.workspace {
                    self.members.contains(package) && !cargo.exclude.contains(package)
                } else {
                    cargo.packages.contains(package)
                }
            })
    }

    /// The one filterset a draft lane adds to its command.
    pub(crate) fn filterset(&self) -> String {
        self.packages
            .iter()
            .map(|package| format!("package({package})"))
            .chain(self.filter.iter().map(|filter| format!("({filter})")))
            .collect::<Vec<_>>()
            .join(" | ")
    }
}

/// Checks a pull request's test block against the workspace at `root`: the
/// shape, every package it names, and its filterset.
///
/// # Errors
///
/// Returns the first thing wrong with the block, or why the workspace could
/// not be read.
pub fn validate_pr_tests(root: &Path, block: &str) -> Result<()> {
    let (graph, named) = read_scope(root, Some(block))?;
    let members = graph
        .workspace()
        .iter()
        .map(|package| package.name().to_owned())
        .collect();
    validate_block(&graph, &members, &named)
}

fn read_scope(root: &Path, block: Option<&str>) -> Result<(PackageGraph, PrTests)> {
    let named: PrTests = toml::from_str(block.unwrap_or_default())
        .context("the pull request's kithara-tests block")?;
    let mut command = MetadataCommand::new();
    command.current_dir(root);
    // Package lists need only members; a filter may query dependency edges.
    if named.filter.is_none() {
        command.no_deps();
    }
    let graph = command
        .build_graph()
        .context("reading the workspace package graph")?;
    Ok((graph, named))
}

fn validate_block(graph: &PackageGraph, members: &BTreeSet<String>, named: &PrTests) -> Result<()> {
    for package in &named.packages {
        if !members.contains(package) {
            bail!(
                "the pull request's kithara-tests block names `{package}`, which is not a workspace package"
            );
        }
    }
    if let Some(filter) = &named.filter {
        Filterset::parse(
            filter.clone(),
            &ParseContext::new(graph),
            FiltersetKind::Test,
            &KnownGroups::Known {
                custom_groups: HashSet::new(),
            },
        )
        .map_err(|errors| {
            let reasons = errors
                .errors
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; ");
            anyhow::anyhow!(
                "the pull request's kithara-tests filter `{filter}` is not a filterset nextest accepts: {reasons}"
            )
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{fs, sync::OnceLock};

    use super::*;
    use crate::{
        common::project::TestRequiredConfig, test::repository_tests::root as workspace_root,
    };

    fn graph() -> &'static PackageGraph {
        static GRAPH: OnceLock<PackageGraph> = OnceLock::new();
        GRAPH.get_or_init(|| {
            read_scope(&workspace_root(), None)
                .expect("the workspace package graph")
                .0
        })
    }

    fn config(required: &[&str]) -> TestCommandConfig {
        TestCommandConfig {
            required: TestRequiredConfig {
                packages: required.iter().map(|name| (*name).to_owned()).collect(),
            },
            ..TestCommandConfig::default()
        }
    }

    fn scope(block: Option<&str>) -> Result<DraftScope> {
        DraftScope::load(&config(&["xtask"]), &workspace_root(), block)
    }

    fn rejected(block: &str) -> String {
        format!(
            "{:#}",
            scope(Some(block)).expect_err("the block is rejected")
        )
    }

    fn packages(names: &[&str]) -> TestCargoOptions {
        TestCargoOptions {
            packages: names.iter().map(|name| (*name).to_owned()).collect(),
            ..TestCargoOptions::default()
        }
    }

    #[test]
    fn no_block_runs_the_required_packages() {
        let scope = scope(None).unwrap();

        assert_eq!(scope.filterset(), "package(xtask)");
    }

    #[test]
    fn package_scope_does_not_resolve_unrequested_dependencies() {
        let temp = tempfile::tempdir().expect("fixture workspace");
        let root = temp.path();
        fs::create_dir(root.join("src")).expect("fixture sources");
        fs::create_dir(root.join(".cargo")).expect("fixture Cargo configuration");
        fs::create_dir(root.join("empty")).expect("empty dependency source");
        fs::write(root.join("src/lib.rs"), "").expect("fixture library");
        fs::write(
            root.join("Cargo.toml"),
            "[workspace]\n[package]\nname = \"probe\"\nversion = \"0.0.0\"\nedition = \"2024\"\n[dependencies]\nnot-in-fixture-registry = \"=0.0.0\"\n",
        )
        .expect("fixture manifest");
        fs::write(
            root.join(".cargo/config.toml"),
            "[source.crates-io]\nreplace-with = \"fixture\"\n[source.fixture]\ndirectory = \"empty\"\n",
        )
        .expect("local dependency source");

        for block in [None, Some("packages = [\"probe\"]")] {
            let scope = DraftScope::load(&config(&["probe"]), root, block)
                .expect("package selection needs only workspace members");
            assert_eq!(scope.filterset(), "package(probe)");
        }

        let error = DraftScope::load(&config(&["probe"]), root, Some("filter = \"deps(probe)\""))
            .expect_err("dependency filters still resolve dependencies");
        assert!(format!("{error:#}").contains("not-in-fixture-registry"));
    }

    #[test]
    fn an_empty_block_asks_for_nothing() {
        assert_eq!(scope(Some("\n")).unwrap().filterset(), "package(xtask)");
    }

    #[test]
    fn a_named_package_joins_the_required_ones() {
        let scope = scope(Some(r#"packages = ["kithara-devtools"]"#)).unwrap();

        assert_eq!(
            scope.filterset(),
            "package(kithara-devtools) | package(xtask)"
        );
    }

    #[test]
    fn a_named_filter_joins_the_required_packages() {
        let scope = scope(Some(r#"filter = "test(/seek_past_end/)""#)).unwrap();

        assert_eq!(
            scope.filterset(),
            "package(xtask) | (test(/seek_past_end/))"
        );
    }

    #[test]
    fn a_block_with_both_keys_names_both() {
        let scope = scope(Some(
            "packages = [\"kithara-devtools\"]\nfilter = \"test(seek)\"",
        ))
        .unwrap();

        assert_eq!(
            scope.filterset(),
            "package(kithara-devtools) | package(xtask) | (test(seek))"
        );
    }

    #[test]
    fn an_unknown_key_is_rejected() {
        assert!(rejected(r#"tests = ["seek"]"#).contains("unknown field"));
    }

    #[test]
    fn an_unknown_package_is_rejected_by_name() {
        assert!(rejected(r#"packages = ["kithara-nothing"]"#).contains("`kithara-nothing`"));
    }

    #[test]
    fn a_filterset_nextest_rejects_is_rejected() {
        assert!(rejected(r#"filter = "test(seek""#).contains("not a filterset nextest accepts"));
    }

    #[test]
    fn a_filterset_naming_an_unknown_package_is_rejected() {
        assert!(
            rejected(r#"filter = "package(kithara-nothing)""#)
                .contains("not a filterset nextest accepts")
        );
    }

    #[test]
    fn a_required_package_outside_the_workspace_is_rejected() {
        let error = DraftScope::of(&config(&["kithara-nothing"]), graph(), PrTests::default())
            .expect_err("the required list names a workspace package");

        assert!(format!("{error:#}").contains("`kithara-nothing`"));
    }

    #[test]
    fn a_lane_holds_the_scope_by_its_packages() {
        let scope = scope(None).unwrap();
        let workspace = TestCargoOptions {
            workspace: true,
            exclude: vec!["xtask".to_owned()],
            ..TestCargoOptions::default()
        };

        assert!(scope.holds(&packages(&["xtask", "kithara-devtools"])));
        assert!(!scope.holds(&packages(&["kithara-devtools"])));
        assert!(!scope.holds(&workspace));
        assert!(scope.holds(&TestCargoOptions {
            workspace: true,
            ..TestCargoOptions::default()
        }));
    }

    #[test]
    fn a_lane_holds_any_block_filterset() {
        let scope = scope(Some(r#"filter = "test(seek)""#)).unwrap();

        assert!(scope.holds(&packages(&["kithara-devtools"])));
    }

    /// The reviewed list names packages of this workspace.
    #[test]
    fn the_repository_required_list_names_workspace_packages() {
        let config = crate::common::project::ProjectConfig::load(&workspace_root())
            .expect("the repository configuration");

        DraftScope::of(&config.test, graph(), PrTests::default()).expect("the required list loads");
    }
}
