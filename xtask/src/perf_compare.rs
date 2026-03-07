use std::{fs, path::Path};

use anyhow::{Result, bail};
use regex::Regex;

/// Parse a pipe-delimited hotpath timing table and extract `(function_name, avg_time)` pairs.
///
/// Expected format:
/// ```text
/// | Function | Calls | Avg | P95 | Total | % Total |
/// | decode_chunk | 1000 | 1.00ms | 1.20ms | 1.00s | 50% |
/// ```
///
/// Column 2 (0-indexed 1) is the function name, column 4 (0-indexed 3) is the avg time.
/// Header rows (where the function column is "Function") and rows with unparseable avg times
/// are skipped.
fn extract_metrics(content: &str) -> Vec<(String, String)> {
    let Ok(time_re) = Regex::new(r"^[0-9]+(\.[0-9]+)?(ns|us|ms|s)$") else {
        return Vec::new();
    };

    let mut metrics = Vec::new();

    for line in content.lines() {
        let columns: Vec<&str> = line.split('|').collect();
        // A valid row like "| a | b | c | d | e | f |" splits into
        // ["", " a ", " b ", " c ", " d ", " e ", " f ", ""]
        // We need at least 5 non-empty segments: columns[1] = name, columns[3] = avg
        if columns.len() < 5 {
            continue;
        }

        let name = columns[1].trim().to_string();
        let avg = columns[3].trim().replace("µs", "us");

        if name.is_empty() || avg.is_empty() {
            continue;
        }

        // Skip header row
        if name.eq_ignore_ascii_case("function") {
            continue;
        }

        // Validate time format
        if !time_re.is_match(&avg) {
            continue;
        }

        metrics.push((name, avg));
    }

    metrics
}

/// Convert a time string like `1.00ms`, `200.00us`, `50ns`, `2.5s` to nanoseconds.
fn to_nanoseconds(value: &str) -> Option<f64> {
    let (num_str, factor) = if let Some(num) = value.strip_suffix("ns") {
        (num, 1.0)
    } else if let Some(num) = value.strip_suffix("us") {
        (num, 1_000.0)
    } else if let Some(num) = value.strip_suffix("ms") {
        (num, 1_000_000.0)
    } else if let Some(num) = value.strip_suffix('s') {
        (num, 1_000_000_000.0)
    } else {
        return None;
    };

    let num: f64 = num_str.parse().ok()?;
    Some(num * factor)
}

pub(crate) fn run(current: &Path, baseline: &Path, threshold: u32) -> Result<()> {
    if !current.exists() {
        bail!("current results file not found: {}", current.display());
    }

    if !baseline.exists() {
        println!("Warning: baseline file not found: {}", baseline.display());
        println!("Run tests on main branch to create baseline");
        return Ok(());
    }

    println!("Comparing performance results...");
    println!("Current:  {}", current.display());
    println!("Baseline: {}", baseline.display());
    println!("Threshold: {threshold}%");
    println!();

    let current_content = fs::read_to_string(current)?;
    let baseline_content = fs::read_to_string(baseline)?;

    let current_metrics = extract_metrics(&current_content);
    let baseline_metrics = extract_metrics(&baseline_content);

    if current_metrics.is_empty() {
        bail!("no parsable metrics found in current file");
    }
    if baseline_metrics.is_empty() {
        bail!("no parsable metrics found in baseline file");
    }

    // Build a map from function name to avg time for the baseline.
    let baseline_map: std::collections::HashMap<&str, &str> = baseline_metrics
        .iter()
        .map(|(name, avg)| (name.as_str(), avg.as_str()))
        .collect();

    println!("Function | Current | Baseline | Change");
    println!("---------|---------|----------|-------");

    let mut regression_found = false;
    let mut comparison_count = 0u32;

    for (name, current_time) in &current_metrics {
        let Some(baseline_time) = baseline_map.get(name.as_str()) else {
            continue;
        };

        let Some(current_ns) = to_nanoseconds(current_time) else {
            continue;
        };
        let Some(baseline_ns) = to_nanoseconds(baseline_time) else {
            continue;
        };

        if baseline_ns <= 0.0 {
            continue;
        }

        let change = ((current_ns - baseline_ns) / baseline_ns) * 100.0;

        println!("{name} | {current_time} | {baseline_time} | {change:.2}%");
        comparison_count += 1;

        if change > f64::from(threshold) {
            println!("  REGRESSION: {name} is {change:.2}% slower (threshold: {threshold}%)");
            regression_found = true;
        }
    }

    println!();
    println!("Comparisons made: {comparison_count}");

    if comparison_count == 0 {
        bail!("no overlapping metrics found between current and baseline");
    }

    if regression_found {
        bail!("Performance regression detected");
    }

    println!("No significant regression detected");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_to_nanoseconds() {
        assert_eq!(to_nanoseconds("50ns"), Some(50.0));
        assert_eq!(to_nanoseconds("1.5us"), Some(1500.0));
        assert_eq!(to_nanoseconds("1.00ms"), Some(1_000_000.0));
        assert_eq!(to_nanoseconds("2.5s"), Some(2_500_000_000.0));
        assert_eq!(to_nanoseconds("invalid"), None);
    }

    #[test]
    fn test_extract_metrics() {
        let input = "| Function | Calls | Avg | P95 | Total | % Total |\n\
                     | decode_chunk | 1000 | 1.00ms | 1.20ms | 1.00s | 50% |\n\
                     | parse_header | 1000 | 200.00us | 250.00us | 0.20s | 50% |";
        let metrics = extract_metrics(input);
        assert_eq!(metrics.len(), 2);
        assert_eq!(metrics[0].0, "decode_chunk");
        assert_eq!(metrics[0].1, "1.00ms");
        assert_eq!(metrics[1].0, "parse_header");
        assert_eq!(metrics[1].1, "200.00us");
    }

    #[test]
    fn test_extract_metrics_skips_header() {
        let input = "| Function | Calls | Avg | P95 | Total | % Total |\n";
        let metrics = extract_metrics(input);
        assert!(metrics.is_empty());
    }

    #[test]
    fn test_extract_metrics_handles_mu_symbol() {
        let input = "| Function | Calls | Avg | P95 | Total | % Total |\n\
                     | fn_a | 100 | 300.00µs | 350.00µs | 0.03s | 100% |";
        let metrics = extract_metrics(input);
        assert_eq!(metrics.len(), 1);
        assert_eq!(metrics[0].1, "300.00us");
    }

    #[test]
    fn test_no_regression() {
        // current is slightly slower but within threshold
        // decode_chunk: 1.05ms vs 1.00ms = 5% (< 10%)
        // parse_header: 190us vs 200us = -5% (faster)
        let baseline = "| Function | Calls | Avg | P95 | Total | % Total |\n\
                        | decode_chunk | 1000 | 1.00ms | 1.20ms | 1.00s | 50% |\n\
                        | parse_header | 1000 | 200.00us | 250.00us | 0.20s | 50% |";
        let current = "| Function | Calls | Avg | P95 | Total | % Total |\n\
                       | decode_chunk | 1000 | 1.05ms | 1.25ms | 1.05s | 50% |\n\
                       | parse_header | 1000 | 190.00us | 240.00us | 0.19s | 50% |";

        let dir = std::env::temp_dir().join("perf_compare_test_no_regression");
        let _ = fs::create_dir_all(&dir);
        let current_path = dir.join("current.txt");
        let baseline_path = dir.join("baseline.txt");
        fs::write(&current_path, current).ok();
        fs::write(&baseline_path, baseline).ok();

        let result = run(&current_path, &baseline_path, 10);
        assert!(result.is_ok());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_regression_detected() {
        // decode_chunk: 1.50ms vs 1.00ms = 50% (> 10%)
        let baseline = "| Function | Calls | Avg | P95 | Total | % Total |\n\
                        | decode_chunk | 1000 | 1.00ms | 1.20ms | 1.00s | 50% |\n\
                        | parse_header | 1000 | 200.00us | 250.00us | 0.20s | 50% |";
        let current = "| Function | Calls | Avg | P95 | Total | % Total |\n\
                       | decode_chunk | 1000 | 1.50ms | 1.90ms | 1.50s | 50% |\n\
                       | parse_header | 1000 | 205.00us | 250.00us | 0.21s | 50% |";

        let dir = std::env::temp_dir().join("perf_compare_test_regression");
        let _ = fs::create_dir_all(&dir);
        let current_path = dir.join("current.txt");
        let baseline_path = dir.join("baseline.txt");
        fs::write(&current_path, current).ok();
        fs::write(&baseline_path, baseline).ok();

        let result = run(&current_path, &baseline_path, 10);
        assert!(result.is_err(), "expected regression to be detected");
        let err = result.err().map(|e| e.to_string()).unwrap_or_default();
        assert!(
            err.contains("Performance regression detected"),
            "expected 'Performance regression detected' error, got: {err}"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_no_overlapping_metrics() {
        // Different function names in current vs baseline
        let baseline = "| Function | Calls | Avg | P95 | Total | % Total |\n\
                        | decode_chunk | 1000 | 1.00ms | 1.20ms | 1.00s | 50% |";
        let current = "| Function | Calls | Avg | P95 | Total | % Total |\n\
                       | unrelated_fn | 1000 | 50.00us | 60.00us | 0.05s | 100% |";

        let dir = std::env::temp_dir().join("perf_compare_test_no_overlap");
        let _ = fs::create_dir_all(&dir);
        let current_path = dir.join("current.txt");
        let baseline_path = dir.join("baseline.txt");
        fs::write(&current_path, current).ok();
        fs::write(&baseline_path, baseline).ok();

        let result = run(&current_path, &baseline_path, 10);
        assert!(result.is_err());
        let err = result.err().map(|e| e.to_string()).unwrap_or_default();
        assert!(
            err.contains("no overlapping metrics"),
            "expected 'no overlapping metrics' error, got: {err}"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_missing_baseline_returns_ok() {
        let dir = std::env::temp_dir().join("perf_compare_test_missing_baseline");
        let _ = fs::create_dir_all(&dir);
        let current_path = dir.join("current.txt");
        let baseline_path = dir.join("nonexistent_baseline.txt");
        let content = "| Function | Calls | Avg | P95 | Total | % Total |\n\
                       | decode_chunk | 1000 | 1.00ms | 1.20ms | 1.00s | 50% |";
        fs::write(&current_path, content).ok();

        let result = run(&current_path, &baseline_path, 10);
        assert!(result.is_ok());

        let _ = fs::remove_dir_all(&dir);
    }
}
