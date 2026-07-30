use std::{fs, path::Path};

use anyhow::{Context, Result, anyhow};
use serde_json::Value;

use super::adapter::{InvocationSpec, ReportKind};

pub(super) fn validate_report(path: &Path, invocation: &InvocationSpec) -> Result<bool> {
    let text =
        fs::read_to_string(path).with_context(|| format!("read report: {}", path.display()))?;
    match invocation.report {
        ReportKind::CargoCrapDelta { threshold } => cargo_crap_delta_findings(&text, threshold),
        ReportKind::Json => {
            serde_json::from_str::<Value>(&text).context("parse JSON report")?;
            Ok(false)
        }
        ReportKind::JsonStream => cargo_dupes_findings(&text),
        ReportKind::RustqualJson => rustqual_findings(&text),
    }
}

fn cargo_crap_delta_findings(text: &str, threshold: f64) -> Result<bool> {
    let report: Value = serde_json::from_str(text).context("parse cargo-crap delta JSON report")?;
    let entries = report
        .get("entries")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("cargo-crap delta JSON report omits entries"))?;
    entries.iter().try_fold(false, |has_findings, entry| {
        let status = entry
            .get("status")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("cargo-crap delta entry omits status"))?;
        let crap = entry
            .get("crap")
            .and_then(Value::as_f64)
            .ok_or_else(|| anyhow!("cargo-crap delta entry omits crap"))?;
        Ok(has_findings || status == "regressed" || (status == "new" && crap > threshold))
    })
}

fn rustqual_findings(text: &str) -> Result<bool> {
    let report: Value = serde_json::from_str(text).context("parse rustqual JSON report")?;
    let summary = report
        .get("summary")
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow!("rustqual JSON report omits summary"))?;
    [
        "tq_no_assertion_warnings",
        "tq_no_sut_warnings",
        "tq_untested_warnings",
        "tq_uncovered_warnings",
        "tq_untested_logic_warnings",
    ]
    .into_iter()
    .try_fold(false, |has_findings, field| {
        let count = summary
            .get(field)
            .and_then(Value::as_u64)
            .ok_or_else(|| anyhow!("rustqual summary omits {field}"))?;
        Ok(has_findings || count > 0)
    })
}

fn cargo_dupes_findings(text: &str) -> Result<bool> {
    let mut values = serde_json::Deserializer::from_str(text).into_iter::<Value>();
    let stats = values
        .next()
        .transpose()
        .context("parse cargo-dupes statistics")?
        .ok_or_else(|| anyhow!("cargo-dupes JSON stream is empty"))?;
    for value in values {
        value.context("parse cargo-dupes JSON stream")?;
    }
    let stats = stats
        .as_object()
        .ok_or_else(|| anyhow!("cargo-dupes statistics must be the first JSON object"))?;
    let exact = stats
        .get("sub_exact_groups")
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow!("cargo-dupes statistics omit sub_exact_groups"))?;
    let near = stats
        .get("sub_near_groups")
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow!("cargo-dupes statistics omit sub_near_groups"))?;
    Ok(exact + near > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cargo_dupes_uses_only_sub_function_group_counts() {
        let clean = r#"{"exact_groups":9,"near_groups":7,"sub_exact_groups":0,"sub_near_groups":0}
[]
[]"#;
        let findings = r#"{"exact_groups":0,"near_groups":0,"sub_exact_groups":1,"sub_near_groups":0}
[{"kind":"sub-exact"}]
[]"#;

        assert!(!cargo_dupes_findings(clean).expect("clean stream"));
        assert!(cargo_dupes_findings(findings).expect("findings stream"));
    }

    #[test]
    fn rustqual_uses_only_test_quality_counts() {
        let clean = r#"{
            "summary": {
                "violations": 900,
                "tq_no_assertion_warnings": 0,
                "tq_no_sut_warnings": 0,
                "tq_untested_warnings": 0,
                "tq_uncovered_warnings": 0,
                "tq_untested_logic_warnings": 0
            }
        }"#;
        let findings = clean.replace("\"tq_no_sut_warnings\": 0", "\"tq_no_sut_warnings\": 1");

        assert!(!rustqual_findings(clean).expect("clean rustqual report"));
        assert!(rustqual_findings(&findings).expect("rustqual findings"));
    }

    #[test]
    fn cargo_crap_delta_gates_regressions_and_new_functions_above_threshold() {
        let clean = r#"{"entries":[
            {"status":"new","crap":30.0},
            {"status":"improved","crap":80.0},
            {"status":"unchanged","crap":90.0}
        ],"removed":[]}"#;
        let regression = r#"{"entries":[{"status":"regressed","crap":12.0}],"removed":[]}"#;
        let new_above = r#"{"entries":[{"status":"new","crap":30.1}],"removed":[]}"#;

        assert!(!cargo_crap_delta_findings(clean, 30.0).expect("clean delta"));
        assert!(cargo_crap_delta_findings(regression, 30.0).expect("regression"));
        assert!(cargo_crap_delta_findings(new_above, 30.0).expect("new above threshold"));
    }
}
