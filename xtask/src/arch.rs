use std::{
    collections::{HashMap, HashSet, VecDeque},
    fs,
    path::Path,
};

use anyhow::{Result, bail};
use cargo_metadata::{DependencyKind, MetadataCommand};
use regex::Regex;

use crate::util::walk_rs_files;

/// Canonical types that must have exactly one definition across `crates/`.
const CANONICAL_TYPES: &[(&str, &str)] = &[
    ("enum", "AudioCodec"),
    ("enum", "ContainerFormat"),
    ("struct", "MediaInfo"),
];

/// Base crates that must NOT depend on higher-level crates.
const BASE_CRATES: &[&str] = &["kithara-platform", "kithara-abr", "kithara-drm"];

/// Higher-level crates that base crates must not reach.
const HIGH_CRATES: &[&str] = &[
    "kithara-hls",
    "kithara-file",
    "kithara-audio",
    "kithara-wasm",
    "kithara-decode",
];

/// Mid-level crates that must NOT depend on the facade crate.
const MID_CRATES: &[&str] = &[
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

/// The facade crate name.
const FACADE_CRATE: &str = "kithara";

pub(crate) fn run() -> Result<()> {
    let metadata = MetadataCommand::new().exec()?;
    let workspace_root = metadata.workspace_root.as_std_path();

    let mut errors = 0u32;

    errors += check_canonical_types(workspace_root)?;
    errors += check_duplicate_error_enums(workspace_root)?;
    errors += check_dependency_direction(&metadata);
    errors += check_facade_boundary(&metadata);
    errors += check_stray_rs_files(workspace_root)?;

    if errors > 0 {
        bail!("{errors} architecture violation(s) found");
    }
    println!("OK: no architecture violations.");
    Ok(())
}

/// Check 1: Canonical types must have exactly one definition.
fn check_canonical_types(workspace_root: &Path) -> Result<u32> {
    let crates_dir = workspace_root.join("crates");
    let rs_files = walk_rs_files(&crates_dir)?;
    let mut errors = 0u32;

    for &(kind, name) in CANONICAL_TYPES {
        let pattern = Regex::new(&format!(r"\bpub\s+{kind}\s+{name}\b"))?;
        let mut matches: Vec<(String, usize, String)> = Vec::new();

        for file_path in &rs_files {
            let content = fs::read_to_string(file_path)?;
            for (line_no, line) in content.lines().enumerate() {
                let trimmed = line.trim();
                // Skip comment lines
                if trimmed.starts_with("//") {
                    continue;
                }
                // Skip re-exports
                if trimmed.contains("pub use") {
                    continue;
                }
                if pattern.is_match(line) {
                    let display_path = file_path
                        .strip_prefix(workspace_root)
                        .unwrap_or(file_path)
                        .display()
                        .to_string();
                    matches.push((display_path, line_no + 1, line.to_string()));
                }
            }
        }

        if matches.len() > 1 {
            println!(
                "ERROR: '{kind} {name}' defined {} times (expected 1):",
                matches.len()
            );
            for (file, line_no, line) in &matches {
                println!("{file}:{line_no}:{line}");
            }
            errors += 1;
        }
    }

    Ok(errors)
}

/// Check 2: Warn if any single crate defines `pub enum Error` more than once.
fn check_duplicate_error_enums(workspace_root: &Path) -> Result<u32> {
    let crates_dir = workspace_root.join("crates");
    let rs_files = walk_rs_files(&crates_dir)?;
    let pattern = Regex::new(r"\bpub\s+enum\s+Error\b")?;
    let mut crate_counts: HashMap<String, u32> = HashMap::new();

    for file_path in &rs_files {
        let content = fs::read_to_string(file_path)?;
        for line in content.lines() {
            if pattern.is_match(line) {
                // Extract crate name from path: crates/<crate-name>/...
                let rel = file_path.strip_prefix(&crates_dir).unwrap_or(file_path);
                if let Some(crate_name) = rel.components().next() {
                    let name = crate_name.as_os_str().to_string_lossy().to_string();
                    *crate_counts.entry(name).or_insert(0) += 1;
                }
            }
        }
    }

    for (crate_name, count) in &crate_counts {
        if *count > 1 {
            println!("WARNING: crate '{crate_name}' defines 'pub enum Error' multiple times");
        }
    }

    // Warnings do not count as errors (matching bash behavior)
    Ok(0)
}

/// Collect all transitive dependencies of a package by name, using the resolved graph.
fn transitive_deps(pkg_name: &str, metadata: &cargo_metadata::Metadata) -> HashSet<String> {
    let Some(resolve) = &metadata.resolve else {
        return HashSet::new();
    };

    // Build a map from package id to node for efficient lookup
    let node_map: HashMap<&str, &cargo_metadata::Node> = resolve
        .nodes
        .iter()
        .map(|n| (n.id.repr.as_str(), n))
        .collect();

    // Build a map from package name to package id (for workspace members)
    let pkg_id_map: HashMap<&str, &str> = metadata
        .packages
        .iter()
        .map(|p| (p.name.as_str(), p.id.repr.as_str()))
        .collect();

    let start_id = match pkg_id_map.get(pkg_name) {
        Some(id) => *id,
        None => return HashSet::new(),
    };

    let mut visited: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<&str> = VecDeque::new();
    queue.push_back(start_id);

    while let Some(current_id) = queue.pop_front() {
        if let Some(node) = node_map.get(current_id) {
            for dep in &node.deps {
                // Only follow normal (non-dev, non-build) dependencies.
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

    // Convert package IDs back to names
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

/// Check 3: Base crates must not depend (directly or transitively) on high-level crates.
fn check_dependency_direction(metadata: &cargo_metadata::Metadata) -> u32 {
    let mut errors = 0u32;
    let high_set: HashSet<&str> = HIGH_CRATES.iter().copied().collect();

    for &base in BASE_CRATES {
        let all_deps = transitive_deps(base, metadata);
        for dep_name in &all_deps {
            if high_set.contains(dep_name.as_str()) {
                println!("ERROR: base crate '{base}' depends on higher-level crate '{dep_name}'");
                errors += 1;
            }
        }
    }

    errors
}

/// Check 4: Mid-level crates must not depend on the facade crate.
fn check_facade_boundary(metadata: &cargo_metadata::Metadata) -> u32 {
    let mut errors = 0u32;

    for &mid in MID_CRATES {
        let all_deps = transitive_deps(mid, metadata);
        if all_deps.contains(FACADE_CRATE) {
            println!("ERROR: crate '{mid}' depends on facade crate '{FACADE_CRATE}'");
            errors += 1;
        }
    }

    errors
}

/// Check 5: No `.rs` files at repository root.
fn check_stray_rs_files(workspace_root: &Path) -> Result<u32> {
    let mut errors = 0u32;
    let entries = fs::read_dir(workspace_root)?;
    let mut stray_files: Vec<String> = Vec::new();

    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path.is_file() && path.extension().and_then(|e| e.to_str()) == Some("rs") {
            stray_files.push(path.display().to_string());
        }
    }

    if !stray_files.is_empty() {
        println!("ERROR: .rs files found at repository root:");
        for f in &stray_files {
            println!("{f}");
        }
        errors += 1;
    }

    Ok(errors)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transitive_deps_returns_empty_for_unknown_crate() {
        let metadata = MetadataCommand::new().exec().expect("cargo metadata");
        let deps = transitive_deps("nonexistent-crate-xyz", &metadata);
        assert!(deps.is_empty());
    }

    #[test]
    fn transitive_deps_platform_does_not_include_high_crates() {
        let metadata = MetadataCommand::new().exec().expect("cargo metadata");
        let high_set: HashSet<&str> = HIGH_CRATES.iter().copied().collect();

        for &base in BASE_CRATES {
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

        for &mid in MID_CRATES {
            let deps = transitive_deps(mid, &metadata);
            assert!(
                !deps.contains(FACADE_CRATE),
                "mid crate '{mid}' transitively depends on facade crate '{FACADE_CRATE}'"
            );
        }
    }

    #[test]
    fn transitive_deps_includes_known_dependency() {
        // kithara-hls should depend on kithara-stream (a known relationship)
        let metadata = MetadataCommand::new().exec().expect("cargo metadata");
        let deps = transitive_deps("kithara-hls", &metadata);
        assert!(
            deps.contains("kithara-stream"),
            "kithara-hls should depend on kithara-stream"
        );
    }
}
