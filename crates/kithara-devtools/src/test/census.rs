use std::{collections::BTreeMap, fs, path::PathBuf};

use anyhow::{Context, Result, bail};
use clap::Args;
use serde::Deserialize;

use super::inventory::{TestId, complete_inventory};

/// Compare complete nextest inventories across a test-package split.
#[derive(Debug, Args)]
pub struct CensusArgs {
    /// JSON from `cargo nextest list` at the base revision.
    #[arg(long)]
    before: PathBuf,
    /// JSON from the same nextest selection at the candidate revision.
    #[arg(long)]
    after: PathBuf,
    /// TOML binary rename table: `[binaries]` maps old IDs to successor IDs.
    #[arg(long)]
    renames: PathBuf,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Renames {
    binaries: BTreeMap<String, Vec<String>>,
}

pub(crate) fn run(args: &CensusArgs) -> Result<()> {
    let before = fs::read_to_string(&args.before)
        .with_context(|| format!("read base census {}", args.before.display()))?;
    let after = fs::read_to_string(&args.after)
        .with_context(|| format!("read candidate census {}", args.after.display()))?;
    let renames = fs::read_to_string(&args.renames)
        .with_context(|| format!("read census renames {}", args.renames.display()))?;
    let count = compare(&before, &after, &renames)?;
    tracing::info!(
        tests = count,
        "test census preserved every identity and ignored flag"
    );
    Ok(())
}

fn compare(before: &str, after: &str, renames: &str) -> Result<usize> {
    let before = complete_inventory(before)?;
    let after = complete_inventory(after)?;
    let renames: Renames = toml::from_str(renames).context("parse census binary rename table")?;
    let mut reverse = BTreeMap::new();
    for (source, successors) in &renames.binaries {
        if !before.contains_key(source) {
            bail!("census rename names an unknown base binary `{source}`");
        }
        if successors.is_empty() {
            bail!("census binary `{source}` names no successor");
        }
        for successor in successors {
            if !after.contains_key(successor) {
                bail!("census rename names an unknown candidate binary `{successor}`");
            }
            if reverse
                .insert(successor.as_str(), source.as_str())
                .is_some()
            {
                bail!("census successor `{successor}` is mapped more than once");
            }
        }
    }
    let mut expected = BTreeMap::new();
    for (binary, tests) in before {
        for (name, ignored) in tests {
            expected.insert((binary.clone(), name), ignored);
        }
    }
    if expected.is_empty() {
        bail!("base census contains no tests");
    }
    let mut actual = BTreeMap::new();
    for (binary, tests) in after {
        let source = reverse.get(binary.as_str()).copied().unwrap_or(&binary);
        for (name, ignored) in tests {
            let id = (source.to_owned(), name);
            if actual.insert(id.clone(), ignored).is_some() {
                bail!(
                    "census duplicates `{}\t{}` across successor binaries",
                    id.0,
                    id.1
                );
            }
        }
    }
    let missing: Vec<&TestId> = expected
        .keys()
        .filter(|id| !actual.contains_key(*id))
        .collect();
    let added: Vec<&TestId> = actual
        .keys()
        .filter(|id| !expected.contains_key(*id))
        .collect();
    let changed: Vec<&TestId> = expected
        .iter()
        .filter_map(|(id, ignored)| actual.get(id).filter(|value| *value != ignored).map(|_| id))
        .collect();
    if !missing.is_empty() || !added.is_empty() || !changed.is_empty() {
        let rows = |ids: &[&TestId]| {
            ids.iter()
                .map(|(binary, name)| format!("{binary}\t{name}"))
                .collect::<Vec<_>>()
                .join("\n")
        };
        bail!(
            "test census differs: base {}, candidate {}\nmissing ({}):\n{}\nadded ({}):\n{}\nchanged ignored flag ({}):\n{}",
            expected.len(),
            actual.len(),
            missing.len(),
            rows(&missing),
            added.len(),
            rows(&added),
            changed.len(),
            rows(&changed),
        );
    }
    Ok(expected.len())
}

#[cfg(test)]
#[path = "census_tests.rs"]
mod tests;
