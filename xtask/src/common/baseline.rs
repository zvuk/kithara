//! Ratchet baseline: known violation counts per (check, key).
//!
//! The baseline file caps regressions: any observed count strictly greater
//! than the recorded one is a failure; equal-or-less is allowed (improvements
//! are reported but do not fail). `--update-baseline` rewrites the file from
//! the current observation set.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::violation::{Report, Severity, Violation};

const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Default, Deserialize, Serialize)]
pub(crate) struct Baseline {
    #[serde(default = "default_schema_version")]
    pub(crate) schema_version: u32,
    /// Per-check entries: `check_id` → (`key` → recorded count).
    /// Cannot use `deny_unknown_fields` here because `serde(flatten)` over a
    /// map collects all remaining fields — those are the per-check sections.
    #[serde(flatten)]
    pub(crate) checks: BTreeMap<String, BTreeMap<String, u64>>,
}

fn default_schema_version() -> u32 {
    SCHEMA_VERSION
}

impl Baseline {
    pub(crate) fn path(config_dir: &Path) -> PathBuf {
        config_dir.join("baseline.toml")
    }

    pub(crate) fn load(config_dir: &Path) -> Result<Self> {
        let path = Self::path(config_dir);
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = fs::read_to_string(&path)
            .with_context(|| format!("read baseline: {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("parse baseline: {}", path.display()))
    }

    /// Return a copy of this baseline with only entries whose key passes
    /// the predicate. Used to drop out-of-scope baseline entries before
    /// ratchet diffing on a scoped run.
    pub(crate) fn filter_keys(&self, predicate: impl Fn(&str) -> bool) -> Self {
        let mut out = Self {
            schema_version: self.schema_version,
            checks: BTreeMap::new(),
        };
        for (check, keys) in &self.checks {
            let kept: BTreeMap<String, u64> = keys
                .iter()
                .filter(|(k, _)| predicate(k.as_str()))
                .map(|(k, v)| (k.clone(), *v))
                .collect();
            if !kept.is_empty() {
                out.checks.insert(check.clone(), kept);
            }
        }
        out
    }

    /// Build a fresh baseline from the report's current observations.
    pub(crate) fn from_report(report: &Report) -> Self {
        let mut checks: BTreeMap<String, BTreeMap<String, u64>> = BTreeMap::new();
        for v in &report.violations {
            *checks
                .entry(v.check.to_string())
                .or_default()
                .entry(v.key.clone())
                .or_insert(0) += 1;
        }
        Self {
            schema_version: SCHEMA_VERSION,
            checks,
        }
    }

    pub(crate) fn save(&self, config_dir: &Path) -> Result<PathBuf> {
        fs::create_dir_all(config_dir)
            .with_context(|| format!("create config dir: {}", config_dir.display()))?;
        let path = Self::path(config_dir);
        let mut text = toml::to_string_pretty(self).context("serialize baseline")?;
        if !text.ends_with('\n') {
            text.push('\n');
        }
        fs::write(&path, text).with_context(|| format!("write baseline: {}", path.display()))?;
        Ok(path)
    }

    /// Compare observations against this baseline.
    pub(crate) fn diff<'a>(&self, observed: &'a [Violation]) -> RatchetDiff<'a> {
        // observed counts by (check, key)
        let mut observed_counts: BTreeMap<(&str, &str), u64> = BTreeMap::new();
        for v in observed {
            *observed_counts
                .entry((v.check, v.key.as_str()))
                .or_insert(0) += 1;
        }

        let mut regressions: Vec<&'a Violation> = Vec::new();
        let mut new_violations: Vec<&'a Violation> = Vec::new();
        let mut improvements: Vec<Improvement> = Vec::new();

        // first pass: compare observed against baseline
        for v in observed {
            let baseline_count = self
                .checks
                .get(v.check)
                .and_then(|m| m.get(&v.key))
                .copied()
                .unwrap_or(0);
            let observed_count = observed_counts[&(v.check, v.key.as_str())];
            if baseline_count == 0 {
                if v.severity == Severity::Deny {
                    new_violations.push(v);
                }
                // warn-level violations are tolerated when not baselined.
            } else if observed_count > baseline_count {
                regressions.push(v);
            }
        }

        // second pass: keys in baseline that disappeared or shrank
        for (check, keys) in &self.checks {
            for (key, &recorded) in keys {
                let observed_count = observed_counts
                    .get(&(check.as_str(), key.as_str()))
                    .copied()
                    .unwrap_or(0);
                if observed_count < recorded {
                    improvements.push(Improvement {
                        check: check.clone(),
                        key: key.clone(),
                        from: recorded,
                        to: observed_count,
                    });
                }
            }
        }

        // dedup regressions/new (a violation may have been pushed once per occurrence)
        regressions.sort_by_key(|v| (v.check, v.key.as_str()));
        regressions.dedup_by(|a, b| a.check == b.check && a.key == b.key);
        new_violations.sort_by_key(|v| (v.check, v.key.as_str()));
        new_violations.dedup_by(|a, b| a.check == b.check && a.key == b.key);

        RatchetDiff {
            regressions,
            new_violations,
            improvements,
        }
    }
}

#[derive(Debug)]
pub(crate) struct RatchetDiff<'a> {
    pub(crate) regressions: Vec<&'a Violation>,
    pub(crate) new_violations: Vec<&'a Violation>,
    pub(crate) improvements: Vec<Improvement>,
}

impl RatchetDiff<'_> {
    pub(crate) fn has_failures(&self) -> bool {
        !self.regressions.is_empty() || !self.new_violations.is_empty()
    }
}

#[derive(Debug)]
pub(crate) struct Improvement {
    pub(crate) check: String,
    pub(crate) key: String,
    pub(crate) from: u64,
    pub(crate) to: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deny(check: &'static str, key: &str) -> Violation {
        Violation::deny(check, key, "test")
    }

    fn warn(check: &'static str, key: &str) -> Violation {
        Violation::warn(check, key, "test")
    }

    fn baseline_with(entries: &[(&str, &str, u64)]) -> Baseline {
        let mut b = Baseline::default();
        for (check, key, count) in entries {
            b.checks
                .entry((*check).to_string())
                .or_default()
                .insert((*key).to_string(), *count);
        }
        b
    }

    #[test]
    fn diff_empty_observation_against_empty_baseline_passes() {
        let b = Baseline::default();
        let diff = b.diff(&[]);
        assert!(diff.regressions.is_empty());
        assert!(diff.new_violations.is_empty());
        assert!(diff.improvements.is_empty());
        assert!(!diff.has_failures());
    }

    #[test]
    fn new_deny_violation_without_baseline_fails() {
        let b = Baseline::default();
        let observed = vec![deny("file_size", "crates/foo/src/big.rs")];
        let diff = b.diff(&observed);
        assert_eq!(diff.new_violations.len(), 1);
        assert!(diff.has_failures());
    }

    #[test]
    fn new_warn_violation_without_baseline_passes() {
        let b = Baseline::default();
        let observed = vec![warn("duplicate_error_enums", "kithara-foo")];
        let diff = b.diff(&observed);
        assert!(diff.new_violations.is_empty());
        assert!(!diff.has_failures());
    }

    #[test]
    fn observed_at_baseline_passes() {
        let b = baseline_with(&[("file_size", "crates/foo/big.rs", 1)]);
        let observed = vec![deny("file_size", "crates/foo/big.rs")];
        let diff = b.diff(&observed);
        assert!(!diff.has_failures());
    }

    #[test]
    fn observed_below_baseline_reports_improvement() {
        let b = baseline_with(&[("flat_directory", "crates/foo/src/impls", 18)]);
        let diff = b.diff(&[]);
        assert!(!diff.has_failures());
        assert_eq!(diff.improvements.len(), 1);
        assert_eq!(diff.improvements[0].from, 18);
        assert_eq!(diff.improvements[0].to, 0);
    }

    #[test]
    fn observed_above_baseline_is_regression() {
        let b = baseline_with(&[("shared_state", "crates/foo/big.rs", 3)]);
        let observed = vec![
            deny("shared_state", "crates/foo/big.rs"),
            deny("shared_state", "crates/foo/big.rs"),
            deny("shared_state", "crates/foo/big.rs"),
            deny("shared_state", "crates/foo/big.rs"),
        ];
        let diff = b.diff(&observed);
        assert_eq!(diff.regressions.len(), 1);
        assert!(diff.has_failures());
    }

    #[test]
    fn from_report_counts_occurrences() {
        let mut report = Report::default();
        report.extend([
            deny("file_size", "a.rs"),
            deny("file_size", "b.rs"),
            deny("file_size", "a.rs"),
        ]);
        let b = Baseline::from_report(&report);
        assert_eq!(b.checks["file_size"]["a.rs"], 2);
        assert_eq!(b.checks["file_size"]["b.rs"], 1);
    }

    #[test]
    fn save_then_load_round_trip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut report = Report::default();
        report.extend([
            deny("file_size", "crates/foo/big.rs"),
            warn("duplicate_error_enums", "kithara-foo"),
        ]);
        let written = Baseline::from_report(&report);
        let path = written.save(dir.path()).expect("save");
        assert!(path.exists());
        let loaded = Baseline::load(dir.path()).expect("load");
        assert_eq!(loaded.schema_version, SCHEMA_VERSION);
        assert_eq!(loaded.checks, written.checks);
    }
}
