use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Result, bail};
use cargo_metadata::MetadataCommand;
use clap::Subcommand;
use regex::Regex;

use crate::util::walk_rs_files;

#[derive(Clone, Copy, Debug, Subcommand)]
pub(crate) enum QualityCommand {
    /// Validate workspace architecture.
    Arch,
    /// Generate a quality report.
    Report {
        #[arg(long)]
        min_unimock_traits: Option<usize>,
        #[arg(long)]
        min_rstest_cases: Option<usize>,
        #[arg(long)]
        min_perf_test_files: Option<usize>,
        #[arg(long)]
        min_bench_targets: Option<usize>,
        #[arg(long)]
        max_local_http_servers: Option<usize>,
    },
    /// Audit rstest usage.
    RstestAudit,
    /// Audit trait mocks.
    TraitMockAudit,
    /// List trait-mock exceptions.
    TraitMockExceptions,
    /// Check unimock usage.
    UnimockCheck,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn workspace_root() -> Result<PathBuf> {
    let metadata = MetadataCommand::new().exec()?;
    Ok(metadata.workspace_root.into_std_path_buf())
}

/// Count lines matching `pattern` across the given files.
fn count_pattern(files: &[PathBuf], pattern: &Regex) -> Result<usize> {
    let mut total = 0usize;
    for file in files {
        let content = fs::read_to_string(file)?;
        for line in content.lines() {
            if pattern.is_match(line) {
                total += 1;
            }
        }
    }
    Ok(total)
}

/// Collect `.rs` files from multiple directories (skipping missing ones).
fn collect_rs_files(dirs: &[&Path]) -> Result<Vec<PathBuf>> {
    let mut all = Vec::new();
    for dir in dirs {
        if dir.exists() {
            all.extend(walk_rs_files(dir)?);
        }
    }
    all.sort();
    Ok(all)
}

/// Count `.rs` files under directories whose parent path includes a `benches` component.
fn count_bench_rs_files(dirs: &[&Path]) -> Result<usize> {
    let mut count = 0usize;
    for dir in dirs {
        if !dir.exists() {
            continue;
        }
        for file in walk_rs_files(dir)? {
            let has_benches_parent = file
                .ancestors()
                .any(|a| a.file_name().and_then(|n| n.to_str()) == Some("benches"));
            if has_benches_parent {
                count += 1;
            }
        }
    }
    Ok(count)
}

/// Count `.rs` files in a directory (recursively).
fn count_rs_files_in(dir: &Path) -> Result<usize> {
    if !dir.exists() {
        return Ok(0);
    }
    let files = walk_rs_files(dir)?;
    Ok(files.len())
}

fn utc_timestamp() -> String {
    let now = std::time::SystemTime::now();
    let secs = now
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let days = secs / 86400;
    let time_of_day = secs % 86400;
    let hours = time_of_day / 3600;
    let minutes = (time_of_day % 3600) / 60;
    let seconds = time_of_day % 60;

    let (year, month, day) = epoch_days_to_date(days);

    format!("{year:04}-{month:02}-{day:02}T{hours:02}:{minutes:02}:{seconds:02}Z")
}

/// Convert days since Unix epoch (1970-01-01) to (year, month, day).
/// Uses Howard Hinnant's civil-from-days algorithm.
fn epoch_days_to_date(days: u64) -> (u64, u64, u64) {
    let z = days + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    (year, m, d)
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

pub(crate) fn run(cmd: QualityCommand) -> Result<()> {
    match cmd {
        QualityCommand::Arch => crate::arch::run(),
        QualityCommand::Report {
            min_unimock_traits,
            min_rstest_cases,
            min_perf_test_files,
            min_bench_targets,
            max_local_http_servers,
        } => run_report(
            min_unimock_traits,
            min_rstest_cases,
            min_perf_test_files,
            min_bench_targets,
            max_local_http_servers,
        ),
        QualityCommand::RstestAudit => run_rstest_audit(),
        QualityCommand::TraitMockAudit => run_trait_mock_audit(),
        QualityCommand::TraitMockExceptions => run_trait_mock_exceptions(),
        QualityCommand::UnimockCheck => run_unimock_check(),
    }
}

// ---------------------------------------------------------------------------
// Subcommand 1: report
// ---------------------------------------------------------------------------

fn gate_status(value: usize, threshold: Option<usize>, is_max: bool) -> String {
    threshold.map_or_else(
        || "off | pass".to_string(),
        |t| {
            let pass = if is_max { value <= t } else { value >= t };
            let status = if pass { "pass" } else { "fail" };
            format!("{t} | {status}")
        },
    )
}

fn run_report(
    min_unimock_traits: Option<usize>,
    min_rstest_cases: Option<usize>,
    min_perf_test_files: Option<usize>,
    min_bench_targets: Option<usize>,
    max_local_http_servers: Option<usize>,
) -> Result<()> {
    let root = workspace_root()?;
    let crates_dir = root.join("crates");
    let tests_dir = root.join("tests");
    let target_dir = root.join("target");
    fs::create_dir_all(&target_dir)?;

    let crate_files = collect_rs_files(&[&crates_dir])?;
    let crate_and_test_files = collect_rs_files(&[&crates_dir, &tests_dir])?;
    let test_files = collect_rs_files(&[&tests_dir])?;

    let re_pub_trait = Regex::new(r"pub trait ")?;
    let re_unimock = Regex::new(r"unimock::unimock\(")?;
    let re_rstest = Regex::new(r"#\[rstest\]")?;
    let re_plain_test = Regex::new(r"#\[(tokio::)?test\]")?;
    let re_tcp_listener = Regex::new(r#"TcpListener::bind\("127\.0\.0\.1:0"\)"#)?;

    let total_traits = count_pattern(&crate_files, &re_pub_trait)?;
    let unimock_traits = count_pattern(&crate_files, &re_unimock)?;
    let rstest_cases = count_pattern(&crate_and_test_files, &re_rstest)?;
    let plain_tests = count_pattern(&crate_and_test_files, &re_plain_test)?;
    let perf_tests = count_rs_files_in(&tests_dir.join("perf"))?;
    let bench_targets = count_bench_rs_files(&[&crates_dir, &tests_dir])?;
    let local_http_servers = count_pattern(&test_files, &re_tcp_listener)?;

    // Gate checks
    let mut gate_errors: Vec<String> = Vec::new();

    if let Some(min) = min_unimock_traits
        && unimock_traits < min
    {
        println!("FAILED: unimock traits is {unimock_traits} (minimum: {min})");
        gate_errors.push("unimock_traits".to_string());
    }
    if let Some(min) = min_rstest_cases
        && rstest_cases < min
    {
        println!("FAILED: rstest cases is {rstest_cases} (minimum: {min})");
        gate_errors.push("rstest_cases".to_string());
    }
    if let Some(min) = min_perf_test_files
        && perf_tests < min
    {
        println!("FAILED: perf test files is {perf_tests} (minimum: {min})");
        gate_errors.push("perf_tests".to_string());
    }
    if let Some(min) = min_bench_targets
        && bench_targets < min
    {
        println!("FAILED: bench targets is {bench_targets} (minimum: {min})");
        gate_errors.push("bench_targets".to_string());
    }
    if let Some(max) = max_local_http_servers
        && local_http_servers > max
    {
        println!("FAILED: local HTTP server bind sites is {local_http_servers} (maximum: {max})");
        gate_errors.push("local_http_servers".to_string());
    }

    let timestamp = utc_timestamp();

    let report = format!(
        "\
# Quality Report

- generated_at_utc: {timestamp}

## Inventory

| Metric | Count |
| --- | ---: |
| Traits in crates/ | {total_traits} |
| Traits with unimock::unimock | {unimock_traits} |
| rstest test cases | {rstest_cases} |
| Plain #[test]/#[tokio::test] markers | {plain_tests} |
| perf test files (tests/perf) | {perf_tests} |
| bench targets (*/benches/*.rs) | {bench_targets} |
| Local HTTP server bind sites in tests | {local_http_servers} |

## Gates

| Gate | Threshold | Status |
| --- | ---: | --- |
| Traits with unimock::unimock | {unimock_gate} |
| rstest test cases | {rstest_gate} |
| perf test files | {perf_gate} |
| bench targets | {bench_gate} |
| local HTTP server bind sites | {http_gate} |

## Notes

- Inventory is always generated; gates are optional and env-driven.
- Failing gates exit non-zero to make regressions visible in CI.
",
        unimock_gate = gate_status(unimock_traits, min_unimock_traits, false),
        rstest_gate = gate_status(rstest_cases, min_rstest_cases, false),
        perf_gate = gate_status(perf_tests, min_perf_test_files, false),
        bench_gate = gate_status(bench_targets, min_bench_targets, false),
        http_gate = gate_status(local_http_servers, max_local_http_servers, true),
    );

    let output_path = target_dir.join("quality-report.md");
    fs::write(&output_path, &report)?;
    println!("==> quality report written to {}", output_path.display());

    if !gate_errors.is_empty() {
        let names = gate_errors.join(" ");
        bail!("quality gates failed: {names}");
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Subcommand 2: rstest-audit
// ---------------------------------------------------------------------------

fn run_rstest_audit() -> Result<()> {
    let root = workspace_root()?;
    let crates_dir = root.join("crates");
    let tests_dir = root.join("tests");
    let target_dir = root.join("target");
    fs::create_dir_all(&target_dir)?;

    let files = collect_rs_files(&[&crates_dir, &tests_dir])?;

    let re_plain_test = Regex::new(r"#\[(tokio::)?test\]")?;
    let re_rstest = Regex::new(r"#\[rstest\]")?;

    let mut total_files = 0usize;
    let mut total_plain_tests = 0usize;
    let mut total_rstest_cases = 0usize;
    let mut candidates: Vec<(String, usize)> = Vec::new();

    for file in &files {
        let content = fs::read_to_string(file)?;
        total_files += 1;

        let mut plain = 0usize;
        let mut rstest = 0usize;

        for line in content.lines() {
            if re_plain_test.is_match(line) {
                plain += 1;
            }
            if re_rstest.is_match(line) {
                rstest += 1;
            }
        }

        total_plain_tests += plain;
        total_rstest_cases += rstest;

        if plain >= 3 && rstest == 0 {
            let rel = file
                .strip_prefix(&root)
                .unwrap_or(file)
                .display()
                .to_string();
            candidates.push((rel, plain));
        }
    }

    let timestamp = utc_timestamp();

    let candidate_rows = if candidates.is_empty() {
        "| (none) | 0 |".to_string()
    } else {
        candidates
            .iter()
            .map(|(path, count)| format!("| {path} | {count} |"))
            .collect::<Vec<_>>()
            .join("\n")
    };

    let report = format!(
        "\
# Rstest Audit

- generated_at_utc: {timestamp}

## Summary

| Metric | Count |
| --- | ---: |
| Rust files scanned | {total_files} |
| Plain #[test]/#[tokio::test] markers | {total_plain_tests} |
| #[rstest] markers | {total_rstest_cases} |
| Migration candidates | {candidate_count} |

## Candidates

| File | Plain test markers |
| --- | ---: |
{candidate_rows}
",
        candidate_count = candidates.len(),
    );

    let output_path = target_dir.join("rstest-audit.md");
    fs::write(&output_path, &report)?;

    println!("rstest audit: scanned {total_files} Rust test files");
    println!("rstest audit: report written to {}", output_path.display());

    if candidates.is_empty() {
        println!("rstest audit: no obvious parameterization candidates found");
    } else {
        println!("rstest audit: candidates for rstest migration:");
        for (path, plain) in &candidates {
            println!("  - {path} (plain_tests={plain})");
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Subcommand 3: trait-mock-audit
// ---------------------------------------------------------------------------

struct TraitFileInfo {
    rel_path: String,
    trait_count: usize,
    unimock_count: usize,
}

/// Scan `.rs` files in `crates/` and collect trait file info.
fn scan_trait_files(root: &Path) -> Result<Vec<TraitFileInfo>> {
    let crates_dir = root.join("crates");
    let files = walk_rs_files(&crates_dir)?;

    let re_pub_trait = Regex::new(r"pub trait ")?;
    let re_unimock = Regex::new(r"unimock(::unimock)?\(")?;

    let mut results = Vec::new();

    for file in &files {
        let content = fs::read_to_string(file)?;

        let trait_count = content.lines().filter(|l| re_pub_trait.is_match(l)).count();
        if trait_count == 0 {
            continue;
        }

        let unimock_count = content.lines().filter(|l| re_unimock.is_match(l)).count();
        let rel_path = file
            .strip_prefix(root)
            .unwrap_or(file)
            .display()
            .to_string();

        results.push(TraitFileInfo {
            rel_path,
            trait_count,
            unimock_count,
        });
    }

    Ok(results)
}

fn run_trait_mock_audit() -> Result<()> {
    let root = workspace_root()?;
    let target_dir = root.join("target");
    fs::create_dir_all(&target_dir)?;

    let trait_files = scan_trait_files(&root)?;

    let total_trait_files = trait_files.len();
    let unimock_trait_files = trait_files.iter().filter(|f| f.unimock_count > 0).count();
    let missing_unimock_trait_files = trait_files.iter().filter(|f| f.unimock_count == 0).count();

    let timestamp = utc_timestamp();

    let detail_rows = if trait_files.is_empty() {
        "| (none) | 0 | 0 | n/a |".to_string()
    } else {
        trait_files
            .iter()
            .map(|f| {
                let has = if f.unimock_count > 0 { "yes" } else { "no" };
                format!(
                    "| {} | {} | {} | {} |",
                    f.rel_path, f.trait_count, f.unimock_count, has
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    let report = format!(
        "\
# Trait Mock Audit

- generated_at_utc: {timestamp}

## Summary

| Metric | Count |
| --- | ---: |
| Trait files | {total_trait_files} |
| Trait files with unimock | {unimock_trait_files} |
| Trait files without unimock | {missing_unimock_trait_files} |

## Details

| File | pub trait count | unimock attr count | has unimock |
| --- | ---: | ---: | --- |
{detail_rows}
"
    );

    let output_path = target_dir.join("trait-mock-audit.md");
    fs::write(&output_path, &report)?;
    println!("==> trait mock audit written to {}", output_path.display());

    Ok(())
}

// ---------------------------------------------------------------------------
// Subcommand 4: trait-mock-exceptions
// ---------------------------------------------------------------------------

fn run_trait_mock_exceptions() -> Result<()> {
    let root = workspace_root()?;
    let target_dir = root.join("target");
    fs::create_dir_all(&target_dir)?;

    let exceptions_file = root.join("xtask").join("trait-mock-exceptions.txt");
    if !exceptions_file.exists() {
        bail!("exceptions file not found: {}", exceptions_file.display());
    }

    let content = fs::read_to_string(&exceptions_file)?;
    let mut expected: Vec<(String, String)> = Vec::new();

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        let parts: Vec<&str> = line.splitn(2, '|').collect();
        if parts.len() < 2 {
            bail!("malformed exceptions entry: {line}");
        }

        let path = parts[0].trim().to_string();
        let reason = parts[1].trim().to_string();

        if path.is_empty() || reason.is_empty() {
            bail!("malformed exceptions entry: {line}");
        }

        expected.push((path, reason));
    }

    let trait_files = scan_trait_files(&root)?;
    let actual_missing: Vec<&str> = trait_files
        .iter()
        .filter(|f| f.unimock_count == 0)
        .map(|f| f.rel_path.as_str())
        .collect();
    let actual_missing_set: HashSet<&str> = actual_missing.iter().copied().collect();

    let expected_paths_set: HashSet<&str> = expected.iter().map(|(p, _)| p.as_str()).collect();

    let unexpected_missing: Vec<&str> = actual_missing
        .iter()
        .filter(|p| !expected_paths_set.contains(*p))
        .copied()
        .collect();

    let stale_exceptions: Vec<&str> = expected
        .iter()
        .map(|(p, _)| p.as_str())
        .filter(|p| !actual_missing_set.contains(*p))
        .collect();

    let timestamp = utc_timestamp();

    let mut rows: Vec<String> = Vec::new();
    for (path, reason) in &expected {
        let status = if actual_missing_set.contains(path.as_str()) {
            "expected_missing"
        } else {
            "stale_exception"
        };
        rows.push(format!("| {path} | {reason} | {status} |"));
    }
    for path in &unexpected_missing {
        rows.push(format!(
            "| {path} | missing reason in {} | unexpected_missing |",
            exceptions_file.display()
        ));
    }

    let detail_rows = if rows.is_empty() {
        "| (none) | n/a | n/a |".to_string()
    } else {
        rows.join("\n")
    };

    let report = format!(
        "\
# Trait Mock Exceptions

- generated_at_utc: {timestamp}

## Summary

| Metric | Count |
| --- | ---: |
| Actual trait files without unimock | {actual_missing_count} |
| Declared exceptions | {declared_count} |
| Unexpected missing unimock | {unexpected_count} |
| Stale exceptions | {stale_count} |

## Details

| File | Reason | Status |
| --- | --- | --- |
{detail_rows}
",
        actual_missing_count = actual_missing.len(),
        declared_count = expected.len(),
        unexpected_count = unexpected_missing.len(),
        stale_count = stale_exceptions.len(),
    );

    let output_path = target_dir.join("trait-mock-exceptions.md");
    fs::write(&output_path, &report)?;
    println!(
        "==> trait mock exceptions report written to {}",
        output_path.display()
    );

    if !unexpected_missing.is_empty() {
        println!("FAILED: trait files without unimock and without explicit exceptions:");
        for path in &unexpected_missing {
            println!("  - {path}");
        }
        bail!(
            "{} trait file(s) without unimock and without explicit exceptions",
            unexpected_missing.len()
        );
    }

    if !stale_exceptions.is_empty() {
        println!(
            "FAILED: stale trait mock exceptions (remove from {}):",
            exceptions_file.display()
        );
        for path in &stale_exceptions {
            println!("  - {path}");
        }
        bail!("{} stale trait mock exception(s)", stale_exceptions.len());
    }

    println!("OK: all trait files without unimock are explicitly documented.");
    Ok(())
}

// ---------------------------------------------------------------------------
// Subcommand 5: unimock-check
// ---------------------------------------------------------------------------

fn run_unimock_check() -> Result<()> {
    let root = workspace_root()?;
    let traits_dir = root.join("crates/kithara-play/src/traits");

    if !traits_dir.exists() {
        bail!(
            "kithara-play traits directory not found: {}",
            traits_dir.display()
        );
    }

    let files = walk_rs_files(&traits_dir)?;

    let re_pub_trait = Regex::new(r"pub trait ")?;
    let re_unimock = Regex::new(r"unimock::unimock\(")?;

    let mut checked = 0usize;
    let mut missing: Vec<String> = Vec::new();

    for file in &files {
        let content = fs::read_to_string(file)?;

        let has_trait = content.lines().any(|l| re_pub_trait.is_match(l));
        if !has_trait {
            continue;
        }

        checked += 1;

        let has_unimock = content.lines().any(|l| re_unimock.is_match(l));
        if !has_unimock {
            let rel = file
                .strip_prefix(&root)
                .unwrap_or(file)
                .display()
                .to_string();
            missing.push(rel);
        }
    }

    if !missing.is_empty() {
        println!("FAILED: traits without unimock in kithara-play:");
        for path in &missing {
            println!("  - {path}");
        }
        bail!(
            "{} kithara-play trait file(s) without unimock",
            missing.len()
        );
    }

    println!(
        "OK: kithara-play traits unimock coverage is complete \
         ({checked} files with traits checked)."
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_days_to_date_unix_epoch() {
        let (y, m, d) = epoch_days_to_date(0);
        assert_eq!((y, m, d), (1970, 1, 1));
    }

    #[test]
    fn epoch_days_to_date_known_date() {
        // 2024-01-01 is day 19723
        let (y, m, d) = epoch_days_to_date(19723);
        assert_eq!((y, m, d), (2024, 1, 1));
    }

    #[test]
    fn walk_rs_files_returns_sorted() {
        let root = workspace_root().expect("workspace root");
        let crates_dir = root.join("crates").join("kithara-platform");
        let files = walk_rs_files(&crates_dir).expect("walk_rs_files");
        assert!(!files.is_empty(), "should find .rs files");
        let sorted = {
            let mut v = files.clone();
            v.sort();
            v
        };
        assert_eq!(files, sorted, "files should be sorted");
    }

    #[test]
    fn count_pattern_finds_pub_trait() {
        let root = workspace_root().expect("workspace root");
        let crates_dir = root.join("crates");
        let files = walk_rs_files(&crates_dir).expect("walk_rs_files");
        let re = Regex::new(r"pub trait ").expect("regex");
        let count = count_pattern(&files, &re).expect("count_pattern");
        assert!(count > 0, "should find at least one pub trait");
    }

    #[test]
    fn scan_trait_files_finds_entries() {
        let root = workspace_root().expect("workspace root");
        let entries = scan_trait_files(&root).expect("scan_trait_files");
        assert!(!entries.is_empty(), "should find trait files");
    }

    #[test]
    fn utc_timestamp_has_expected_format() {
        let ts = utc_timestamp();
        let re = Regex::new(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$").expect("regex");
        assert!(re.is_match(&ts), "timestamp should match ISO 8601: {ts}");
    }
}
