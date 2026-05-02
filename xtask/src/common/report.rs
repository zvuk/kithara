//! Render `Report` as markdown or JSON.

use std::collections::BTreeMap;

use super::{
    baseline::RatchetDiff,
    violation::{Report, Severity},
};

pub(crate) fn render_markdown(
    report: &Report,
    ran: &[&'static str],
    diff: &RatchetDiff<'_>,
) -> String {
    let mut out = String::new();
    out.push_str("# Architectural Audit\n\n");
    out.push_str(&format!(
        "- checks run: {}\n- deny violations: {}\n- warn violations: {}\n- new (since baseline): {}\n- regressions: {}\n- improvements: {}\n\n",
        ran.len(),
        report.deny_count(),
        report.warn_count(),
        diff.new_violations.len(),
        diff.regressions.len(),
        diff.improvements.len(),
    ));

    let mut by_check: BTreeMap<&str, Vec<&_>> = BTreeMap::new();
    for v in &report.violations {
        by_check.entry(v.check).or_default().push(v);
    }

    for (check, vs) in by_check {
        out.push_str(&format!("## {check}\n\n"));
        out.push_str("| severity | key | message |\n");
        out.push_str("| --- | --- | --- |\n");
        for v in vs {
            out.push_str(&format!(
                "| {} | `{}` | {} |\n",
                v.severity, v.key, v.message,
            ));
        }
        out.push('\n');
    }

    if !diff.improvements.is_empty() {
        out.push_str("## Ratchet improvements\n\n");
        out.push_str("| check | key | from | to |\n");
        out.push_str("| --- | --- | ---: | ---: |\n");
        for imp in &diff.improvements {
            out.push_str(&format!(
                "| {} | `{}` | {} | {} |\n",
                imp.check, imp.key, imp.from, imp.to,
            ));
        }
        out.push('\n');
    }

    out
}

pub(crate) fn render_json(report: &Report, ran: &[&'static str], diff: &RatchetDiff<'_>) -> String {
    use std::fmt::Write;
    let mut out = String::from("{\n");
    let _ = writeln!(
        out,
        r#"  "checks_run": [{}],"#,
        ran.iter()
            .map(|s| format!("\"{s}\""))
            .collect::<Vec<_>>()
            .join(",")
    );
    let _ = writeln!(out, r#"  "deny": {},"#, report.deny_count());
    let _ = writeln!(out, r#"  "warn": {},"#, report.warn_count());
    let _ = writeln!(out, r#"  "new": {},"#, diff.new_violations.len());
    let _ = writeln!(out, r#"  "regressions": {},"#, diff.regressions.len());
    let _ = writeln!(out, r#"  "improvements": {},"#, diff.improvements.len());
    out.push_str(r#"  "violations": ["#);
    out.push('\n');
    for (i, v) in report.violations.iter().enumerate() {
        let sep = if i + 1 == report.violations.len() {
            ""
        } else {
            ","
        };
        let sev = match v.severity {
            Severity::Warn => "warn",
            Severity::Deny => "deny",
        };
        let _ = writeln!(
            out,
            r#"    {{ "check": "{}", "severity": "{}", "key": "{}", "message": "{}" }}{}"#,
            json_escape(v.check),
            sev,
            json_escape(&v.key),
            json_escape(&v.message),
            sep,
        );
    }
    out.push_str("  ]\n}\n");
    out
}

fn json_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}
