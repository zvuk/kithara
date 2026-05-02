//! Render `Report` as markdown, JSON, or grouped terminal output.

use std::collections::BTreeMap;

use super::{
    baseline::RatchetDiff,
    violation::{Report, Severity, Violation},
};

/// Render a report grouped by `check`. Each check appears once with a
/// `(N deny, M warn)` header; violations follow as `<key> — <message>`
/// lines. Mirrors the JSON/markdown renderers' shape but optimised for
/// a terminal: no per-violation `[SEV] check:` prefix repetition.
pub(crate) fn print_grouped(report: &Report, diff: &RatchetDiff<'_>) {
    if report.violations.is_empty() && diff.improvements.is_empty() {
        return;
    }

    let mut by_check: BTreeMap<&'static str, Vec<&Violation>> = BTreeMap::new();
    for v in &report.violations {
        by_check.entry(v.check).or_default().push(v);
    }

    for (check, vs) in &by_check {
        let deny = vs.iter().filter(|v| v.severity == Severity::Deny).count();
        let warn = vs.iter().filter(|v| v.severity == Severity::Warn).count();
        let counts = match (deny, warn) {
            (0, w) => format!("{w} warn"),
            (d, 0) => format!("{d} deny"),
            (d, w) => format!("{d} deny, {w} warn"),
        };
        println!("{check} ({counts})");

        let mut sorted = vs.clone();
        sorted.sort_by(|a, b| {
            a.severity
                .cmp(&b.severity)
                .reverse()
                .then_with(|| a.key.cmp(&b.key))
        });
        for v in sorted {
            let sev_tag = match v.severity {
                Severity::Deny => "DENY ",
                Severity::Warn => "",
            };
            println!("  {sev_tag}{key} — {msg}", key = v.key, msg = v.message);
        }
    }

    if !diff.improvements.is_empty() {
        println!("ratchet improvements:");
        for imp in &diff.improvements {
            println!(
                "  {check}/{key}: {from} -> {to}",
                check = imp.check,
                key = imp.key,
                from = imp.from,
                to = imp.to,
            );
        }
    }
}

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
