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
pub struct Baseline {
    #[serde(default = "default_schema_version")]
    pub(crate) schema_version: u32,
    /// Per-check entries: `check_id` → (`key` → recorded count).
    /// Cannot use `deny_unknown_fields` here because `serde(flatten)` over a
    /// map collects all remaining fields — those are the per-check sections.
    #[serde(flatten)]
    pub checks: BTreeMap<String, BTreeMap<String, u64>>,
}

fn default_schema_version() -> u32 {
    SCHEMA_VERSION
}

impl Baseline {
    pub(crate) fn path(config_dir: &Path) -> PathBuf {
        config_dir.join("baseline.toml")
    }

    /// Load the ratchet baseline from `config_dir`.
    ///
    /// # Errors
    ///
    /// Returns an error if the baseline file cannot be read or parsed.
    pub fn load(config_dir: &Path) -> Result<Self> {
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
    #[must_use]
    pub fn filter_keys<P>(&self, predicate: P) -> Self
    where
        P: Fn(&str) -> bool,
    {
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
    #[must_use]
    pub fn from_report(report: &Report) -> Self {
        let mut checks: BTreeMap<String, BTreeMap<String, u64>> = BTreeMap::new();
        for v in &report.violations {
            *checks
                .entry(v.check.to_string())
                .or_default()
                .entry(canonical_key(&v.key))
                .or_insert(0) += 1;
        }
        Self {
            schema_version: SCHEMA_VERSION,
            checks,
        }
    }

    /// Save the ratchet baseline into `config_dir`.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory cannot be created, the baseline
    /// cannot be serialized, or the file cannot be written.
    pub fn save(&self, config_dir: &Path) -> Result<PathBuf> {
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
    #[must_use]
    pub fn diff<'a>(&self, observed: &'a [Violation]) -> RatchetDiff<'a> {
        // Match on the canonical (line-insensitive) identity so that edits
        // which merely shift line numbers — e.g. rustfmt re-wrapping an import
        // block above a violation — do not re-fingerprint an unchanged
        // violation as new. See `canonical_key`.
        let mut baseline_counts: BTreeMap<(&str, String), u64> = BTreeMap::new();
        for (check, keys) in &self.checks {
            for (key, &recorded) in keys {
                *baseline_counts
                    .entry((check.as_str(), canonical_key(key)))
                    .or_insert(0) += recorded;
            }
        }
        let mut observed_counts: BTreeMap<(&str, String), u64> = BTreeMap::new();
        for v in observed {
            *observed_counts
                .entry((v.check, canonical_key(&v.key)))
                .or_insert(0) += 1;
        }

        let mut regressions: Vec<&'a Violation> = Vec::new();
        let mut new_violations: Vec<&'a Violation> = Vec::new();
        let mut improvements: Vec<Improvement> = Vec::new();

        for v in observed {
            let ck = canonical_key(&v.key);
            let baseline_count = baseline_counts
                .get(&(v.check, ck.clone()))
                .copied()
                .unwrap_or(0);
            let observed_count = observed_counts[&(v.check, ck)];
            if baseline_count == 0 {
                if v.severity == Severity::Deny {
                    new_violations.push(v);
                }
            } else if observed_count > baseline_count {
                regressions.push(v);
            }
        }

        for ((check, key), &recorded) in &baseline_counts {
            let observed_count = observed_counts
                .get(&(*check, key.clone()))
                .copied()
                .unwrap_or(0);
            if observed_count < recorded {
                improvements.push(Improvement {
                    check: (*check).to_string(),
                    key: key.clone(),
                    from: recorded,
                    to: observed_count,
                });
            }
        }

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

/// Canonical, line-insensitive identity of a violation key for ratchet
/// matching. A key looks like `<rel/path.rs>[:line[:col]][:Type][::name]`.
/// This strips the leading `:line[:col]` numeric segments that follow the file
/// path *iff* a symbolic segment (a type, field, or name) follows them, so a
/// violation keeps its identity when an unrelated edit shifts its line number
/// (e.g. rustfmt re-wrapping an import block above it). Purely positional keys
/// (`path:line:col` with no symbolic tail, such as a loop-body location) are
/// returned unchanged, since the line is their only stable handle.
fn canonical_key(key: &str) -> String {
    let Some((path, rest)) = key.split_once(':') else {
        return key.to_string();
    };
    let segs: Vec<&str> = rest.split(':').collect();
    let lead_numeric = segs
        .iter()
        .take_while(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        .count();
    if lead_numeric == 0 || lead_numeric == segs.len() {
        return key.to_string();
    }
    format!("{path}:{}", segs[lead_numeric..].join(":"))
}

#[derive(Debug)]
pub struct RatchetDiff<'a> {
    pub regressions: Vec<&'a Violation>,
    pub new_violations: Vec<&'a Violation>,
    pub improvements: Vec<Improvement>,
}

impl RatchetDiff<'_> {
    #[must_use]
    pub fn has_failures(&self) -> bool {
        !self.regressions.is_empty() || !self.new_violations.is_empty()
    }
}

#[derive(Debug)]
pub struct Improvement {
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

    #[test]
    fn canonical_key_strips_line_for_named_keys_only() {
        assert_eq!(
            canonical_key("a/b.rs:211:11::live_source_count"),
            "a/b.rs::live_source_count"
        );
        assert_eq!(canonical_key("a/b.rs:101:Foo::bar"), "a/b.rs:Foo::bar");
        assert_eq!(canonical_key("a/b.rs:149:FooImpl"), "a/b.rs:FooImpl");
        // Positional-only key (no symbolic tail): the line is its only handle.
        assert_eq!(canonical_key("a/b.rs:98:33"), "a/b.rs:98:33");
        // No line component, and already-canonical keys, are unchanged.
        assert_eq!(canonical_key("a/b.rs"), "a/b.rs");
        assert_eq!(canonical_key("kithara-foo"), "kithara-foo");
        assert_eq!(
            canonical_key("a/b.rs::live_source_count"),
            "a/b.rs::live_source_count"
        );
    }

    #[test]
    fn line_shift_does_not_refingerprint_named_violation() {
        // A tolerated dead export at line 211 must still match after an edit
        // shifts it to 215 (e.g. rustfmt re-wrapping imports above it): not a
        // new violation, and not an improvement.
        let b = baseline_with(&[(
            "dead_exports",
            "crates/foo/src/flush.rs:211:11::live_source_count",
            1,
        )]);
        let observed = vec![deny(
            "dead_exports",
            "crates/foo/src/flush.rs:215:11::live_source_count",
        )];
        let diff = b.diff(&observed);
        assert!(!diff.has_failures(), "line shift must not fail the ratchet");
        assert!(diff.new_violations.is_empty());
        assert!(diff.improvements.is_empty());
    }

    #[test]
    fn genuinely_new_named_deny_still_fails_after_canonicalization() {
        // Canonicalization must not mask a real new export with a new name.
        let b = baseline_with(&[(
            "dead_exports",
            "crates/foo/src/flush.rs:211:11::live_source_count",
            1,
        )]);
        let observed = vec![deny(
            "dead_exports",
            "crates/foo/src/flush.rs:300:11::brand_new_export",
        )];
        let diff = b.diff(&observed);
        assert_eq!(diff.new_violations.len(), 1);
        assert!(diff.has_failures());
    }
}
