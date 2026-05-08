//! Crate-level direction check: lower-layer crates must not depend on higher.
//!
//! Replaces the legacy `check_dependency_direction` and `check_facade_boundary`
//! by a single layered model loaded from `direction.toml`.

use std::collections::{HashMap, HashSet, VecDeque};

use anyhow::Result;
use cargo_metadata::{DependencyKind, Metadata, Node, Package};

use super::{Check, Context};
use crate::common::{scope::packages_in_scope, violation::Violation};

pub(crate) const ID: &str = "direction";

pub(crate) struct Direction;

impl Check for Direction {
    fn id(&self) -> &'static str {
        ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let cfg = &ctx.config.direction;
        if cfg.layers.is_empty() {
            return Ok(Vec::new());
        }

        let mut crate_to_layer: HashMap<&str, (u32, &str)> = HashMap::new();
        for layer in &cfg.layers {
            for c in &layer.crates {
                crate_to_layer.insert(c.as_str(), (layer.index, layer.name.as_str()));
            }
        }

        let workspace_members: HashSet<&str> = packages_in_scope(ctx.metadata, ctx.scope)
            .iter()
            .map(|p| p.name.as_str())
            .collect();

        let mut violations = Vec::new();
        for member in &workspace_members {
            let Some(&(from_idx, from_name)) = crate_to_layer.get(member) else {
                continue;
            };
            let deps = transitive_deps(member, ctx.metadata);
            for dep in deps {
                let Some(&(to_idx, to_name)) = crate_to_layer.get(dep.as_str()) else {
                    continue;
                };
                if to_idx <= from_idx {
                    continue;
                }
                let key = format!("{member} -> {dep}");
                if cfg.exemptions.contains_key(&key) {
                    continue;
                }
                let message = format!(
                    "crate '{member}' (layer {from_idx} '{from_name}') transitively depends on \
                     '{dep}' (layer {to_idx} '{to_name}'), which is higher in the hierarchy"
                );
                violations.push(Violation::deny(ID, key, message));
            }
        }
        Ok(violations)
    }
}

/// Collect all transitive normal dependencies of a workspace member by name.
pub(crate) fn transitive_deps(pkg_name: &str, metadata: &Metadata) -> HashSet<String> {
    let Some(resolve) = &metadata.resolve else {
        return HashSet::new();
    };

    let node_map: HashMap<&str, &Node> = resolve
        .nodes
        .iter()
        .map(|n| (n.id.repr.as_str(), n))
        .collect();

    let pkg_id_map: HashMap<&str, &str> = metadata
        .packages
        .iter()
        .map(|p: &Package| (p.name.as_str(), p.id.repr.as_str()))
        .collect();

    let Some(start_id) = pkg_id_map.get(pkg_name) else {
        return HashSet::new();
    };

    let mut visited: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<&str> = VecDeque::new();
    queue.push_back(start_id);

    while let Some(current_id) = queue.pop_front() {
        if let Some(node) = node_map.get(current_id) {
            for dep in &node.deps {
                let is_normal = dep
                    .dep_kinds
                    .iter()
                    .any(|dk| dk.kind == DependencyKind::Normal);
                if !is_normal {
                    continue;
                }
                let dep_id = dep.pkg.repr.as_str();
                if visited.insert(dep_id.to_string()) {
                    queue.push_back(dep_id);
                }
            }
        }
    }

    let id_to_name: HashMap<&str, &str> = metadata
        .packages
        .iter()
        .map(|p| (p.id.repr.as_str(), p.name.as_str()))
        .collect();

    visited
        .iter()
        .filter_map(|id| id_to_name.get(id.as_str()).map(ToString::to_string))
        .collect()
}

#[cfg(test)]
mod tests {
    use cargo_metadata::MetadataCommand;

    use super::*;

    /// Hardcoded crate lists from the legacy checks. Kept here as a regression
    /// guard: any future restructuring must not weaken these invariants.
    mod legacy {
        pub(super) const BASE_CRATES: &[&str] = &["kithara-platform", "kithara-abr", "kithara-drm"];
        pub(super) const HIGH_CRATES: &[&str] = &[
            "kithara-hls",
            "kithara-file",
            "kithara-audio",
            "kithara-wasm",
            "kithara-decode",
        ];
        pub(super) const MID_CRATES: &[&str] = &[
            "kithara-storage",
            "kithara-bufpool",
            "kithara-assets",
            "kithara-net",
            "kithara-stream",
            "kithara-decode",
            "kithara-file",
            "kithara-hls",
            "kithara-audio",
            "kithara-events",
        ];
        pub(super) const FACADE_CRATE: &str = "kithara";
    }

    #[test]
    fn transitive_deps_returns_empty_for_unknown_crate() {
        let metadata = MetadataCommand::new().exec().expect("cargo metadata");
        let deps = transitive_deps("nonexistent-crate-xyz", &metadata);
        assert!(deps.is_empty());
    }

    #[test]
    fn transitive_deps_base_does_not_include_high_crates() {
        let metadata = MetadataCommand::new().exec().expect("cargo metadata");
        let high_set: HashSet<&str> = legacy::HIGH_CRATES.iter().copied().collect();

        for &base in legacy::BASE_CRATES {
            let deps = transitive_deps(base, &metadata);
            for dep in &deps {
                assert!(
                    !high_set.contains(dep.as_str()),
                    "base crate '{base}' transitively depends on high-level crate '{dep}'"
                );
            }
        }
    }

    #[test]
    fn transitive_deps_mid_crates_do_not_include_facade() {
        let metadata = MetadataCommand::new().exec().expect("cargo metadata");

        for &mid in legacy::MID_CRATES {
            let deps = transitive_deps(mid, &metadata);
            assert!(
                !deps.contains(legacy::FACADE_CRATE),
                "mid crate '{mid}' transitively depends on facade crate '{facade}'",
                facade = legacy::FACADE_CRATE,
            );
        }
    }

    #[test]
    fn transitive_deps_includes_known_dependency() {
        let metadata = MetadataCommand::new().exec().expect("cargo metadata");
        let deps = transitive_deps("kithara-hls", &metadata);
        assert!(
            deps.contains("kithara-stream"),
            "kithara-hls should depend on kithara-stream"
        );
    }
}
