use std::collections::BTreeSet;

use anyhow::{Context, Result, bail};
use cargo_metadata::Metadata;
use glob::Pattern;
use serde::Serialize;

use super::graph::{EvidenceGraph, NodeId};
use crate::common::project::ArchitectureFilterConfig;

#[derive(Clone, Debug, Default)]
pub(crate) struct ArchitectureFilter {
    exclude_crates: Vec<String>,
    exclude_modules: Vec<String>,
    crate_patterns: Vec<Pattern>,
    module_patterns: Vec<Pattern>,
}

#[derive(Debug, Serialize)]
pub(crate) struct FilterSummary<'a> {
    pub(crate) exclude_crates: &'a [String],
    pub(crate) exclude_modules: &'a [String],
    pub(crate) excluded_nodes: usize,
    pub(crate) excluded_edges: usize,
}

impl ArchitectureFilter {
    pub(crate) fn new(
        config: &ArchitectureFilterConfig,
        cli_crates: &[String],
        cli_modules: &[String],
        metadata: &Metadata,
    ) -> Result<Self> {
        validate_exact_cli_crates(cli_crates, metadata)?;
        let exclude_crates = merged_patterns(&config.exclude_crates, cli_crates);
        let exclude_modules = merged_patterns(&config.exclude_modules, cli_modules);
        Self::compile(exclude_crates, exclude_modules)
    }

    pub(crate) fn compile(
        exclude_crates: Vec<String>,
        exclude_modules: Vec<String>,
    ) -> Result<Self> {
        let crate_patterns = compile_patterns(&exclude_crates, "crate")?;
        let module_patterns = compile_patterns(&exclude_modules, "module")?;
        Ok(Self {
            exclude_crates,
            exclude_modules,
            crate_patterns,
            module_patterns,
        })
    }

    pub(crate) fn excludes(&self, id: &NodeId) -> bool {
        self.excludes_package(&id.package) || self.excludes_module(&id.package, &id.module)
    }

    pub(crate) fn excludes_package(&self, package: &str) -> bool {
        self.crate_patterns
            .iter()
            .any(|pattern| pattern.matches(package))
    }

    pub(crate) fn summary(&self, graph: &EvidenceGraph) -> FilterSummary<'_> {
        FilterSummary {
            exclude_crates: &self.exclude_crates,
            exclude_modules: &self.exclude_modules,
            excluded_nodes: graph.nodes().filter(|node| self.excludes(&node.id)).count(),
            excluded_edges: graph
                .edges()
                .filter(|edge| self.excludes(&edge.source) || self.excludes(&edge.target))
                .count(),
        }
    }

    pub(crate) fn scope_is_fully_excluded(
        &self,
        graph: &EvidenceGraph,
        package: Option<&str>,
        module: &str,
    ) -> bool {
        let mut scoped = graph.nodes().filter(|node| {
            package.is_none_or(|package| node.id.package == package)
                && module_matches(&node.id.module, module)
        });
        let Some(first) = scoped.next() else {
            return false;
        };
        self.excludes(&first.id) && scoped.all(|node| self.excludes(&node.id))
    }

    fn excludes_module(&self, package: &str, module: &str) -> bool {
        let mut canonical = String::with_capacity(package.len() + module.len() + 2);
        canonical.push_str(package);
        canonical.push_str("::");
        canonical.push_str(module);
        loop {
            if self
                .module_patterns
                .iter()
                .any(|pattern| pattern.matches(&canonical))
            {
                return true;
            }
            let Some(separator) = canonical.rfind("::") else {
                return false;
            };
            if separator <= package.len() {
                return false;
            }
            canonical.truncate(separator);
        }
    }
}

fn merged_patterns(config: &[String], cli: &[String]) -> Vec<String> {
    config
        .iter()
        .chain(cli)
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn compile_patterns(patterns: &[String], kind: &str) -> Result<Vec<Pattern>> {
    patterns
        .iter()
        .map(|pattern| {
            if pattern.is_empty() {
                bail!("architecture {kind} exclusion glob cannot be empty");
            }
            Pattern::new(pattern)
                .with_context(|| format!("invalid architecture {kind} exclusion glob `{pattern}`"))
        })
        .collect()
}

fn validate_exact_cli_crates(patterns: &[String], metadata: &Metadata) -> Result<()> {
    let packages = metadata
        .workspace_packages()
        .into_iter()
        .map(|package| package.name.as_str())
        .collect::<BTreeSet<_>>();
    for pattern in patterns
        .iter()
        .filter(|pattern| !contains_glob_meta(pattern))
    {
        if !packages.contains(pattern.as_str()) {
            bail!("workspace package not found for --exclude-crate: {pattern}");
        }
    }
    Ok(())
}

fn contains_glob_meta(pattern: &str) -> bool {
    pattern
        .chars()
        .any(|character| matches!(character, '*' | '?' | '['))
}

fn module_matches(candidate: &str, selected: &str) -> bool {
    candidate == selected
        || candidate
            .strip_prefix(selected)
            .is_some_and(|suffix| suffix.starts_with("::"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_pattern_excludes_the_complete_subtree() {
        let filter = ArchitectureFilter {
            exclude_modules: vec!["demo::tests".to_string()],
            module_patterns: vec![Pattern::new("demo::tests").expect("pattern")],
            ..ArchitectureFilter::default()
        };

        assert!(filter.excludes(&NodeId::module("demo", "demo", "tests")));
        assert!(filter.excludes(&NodeId::module("demo", "demo", "tests::fixtures")));
        assert!(!filter.excludes(&NodeId::module("demo", "demo", "runtime")));
    }

    #[test]
    fn config_and_cli_patterns_merge_deterministically() {
        let merged = merged_patterns(
            &["z-*".to_string(), "shared".to_string()],
            &["a-*".to_string(), "shared".to_string()],
        );

        assert_eq!(merged, ["a-*", "shared", "z-*"]);
    }
}
