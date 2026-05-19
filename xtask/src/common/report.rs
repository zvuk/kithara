use std::collections::BTreeMap;

use super::{
    baseline::RatchetDiff,
    style::{bold, bold_cyan, bold_red, bold_yellow, cyan, dim, green, red},
    violation::{Report, Severity, Violation},
};

const RULE_BAR: &str = "─────────────────────────────────────────────────────────────────────";

/// Map a severity word (`error|deny|warning|warn|info|hint|...`) to a
/// short emoji + colourised tag pair used in block headers.
fn severity_glyphs(severity: &str) -> (&'static str, fn(&str) -> String) {
    match severity {
        "error" | "deny" => ("🛑", bold_red as fn(&str) -> String),
        "warning" | "warn" => ("⚠️ ", bold_yellow as fn(&str) -> String),
        "info" => ("ℹ️ ", bold_cyan as fn(&str) -> String),
        "hint" => ("💡", bold_cyan as fn(&str) -> String),
        _ => ("•", bold as fn(&str) -> String),
    }
}

/// Render a single check/rule block: header, optional rule description,
/// list of locations. Used by both `xtask lint` and the ast-grep wrapper
/// so they share one visual shape.
///
/// `description` — multi-line rationale; printed once, indented two
/// spaces. `locations` yields `(location, fact)` pairs; `fact` is the
/// per-instance message (numbers, threshold details), printed under the
/// location at deeper indent. Pass `None` for `fact` when there is
/// nothing instance-specific to add.
pub(crate) fn print_check_block<I, L>(
    name: &str,
    severity: &str,
    summary: &str,
    description: Option<&str>,
    locations: I,
) where
    I: IntoIterator<Item = (L, Option<String>)>,
    L: AsRef<str>,
{
    let (icon, color) = severity_glyphs(severity);
    let bar_len = RULE_BAR.chars().count();
    let visible_left = icon.chars().count().max(1) + 1 + name.chars().count();
    let pad = bar_len.saturating_sub(visible_left + summary.chars().count() + 2);
    let dots = ".".repeat(pad.max(2));
    println!(
        "{icon} {name_styled} {dots} {sum}",
        name_styled = bold(name),
        dots = dim(&dots),
        sum = color(summary),
    );
    println!("{}", dim(RULE_BAR));

    if let Some(desc) = description {
        let trimmed = desc.trim_matches('\n');
        for line in trimmed.lines() {
            let body = line.trim_end();
            if body.trim().is_empty() {
                println!();
            } else {
                println!("  {}", dim(body.trim_start()));
            }
        }
        println!();
    }

    for (loc, fact) in locations {
        let raw = loc.as_ref();
        let (is_deny, rest) = raw
            .strip_prefix("DENY  ")
            .map_or((false, raw), |stripped| (true, stripped));
        let marker = if is_deny { red("▸") } else { cyan("▸") };
        let location_styled = if is_deny {
            format!("{} {}", bold_red("DENY"), rest)
        } else {
            rest.to_string()
        };
        println!("  {marker} {location_styled}");
        if let Some(f) = fact {
            for line in f.lines() {
                let trimmed = line.trim();
                if !trimmed.is_empty() {
                    println!("      {}", dim(trimmed));
                }
            }
        }
    }
}

/// Render a report grouped by `check`. Each check appears as a block
/// with header, optional rule description (when an explanation is
/// attached to any of its violations), then per-location lines with the
/// instance fact.
pub(crate) fn print_grouped(report: &Report, diff: &RatchetDiff<'_>) {
    if report.violations.is_empty() && diff.improvements.is_empty() {
        return;
    }

    let mut by_check: BTreeMap<&'static str, Vec<&Violation>> = BTreeMap::new();
    for v in &report.violations {
        by_check.entry(v.check).or_default().push(v);
    }

    for (idx, (check, vs)) in by_check.iter().enumerate() {
        if idx > 0 {
            println!();
        }

        let deny = vs.iter().filter(|v| v.severity == Severity::Deny).count();
        let warn = vs.iter().filter(|v| v.severity == Severity::Warn).count();
        let header_severity = if deny > 0 { "deny" } else { "warn" };
        let summary = match (deny, warn) {
            (0, w) => format!("×{w} warn"),
            (d, 0) => format!("×{d} deny"),
            (d, w) => format!("{d} deny + {w} warn"),
        };

        let mut sorted = vs.clone();
        sorted.sort_by(|a, b| {
            a.severity
                .cmp(&b.severity)
                .reverse()
                .then_with(|| a.key.cmp(&b.key))
        });

        let description = sorted.iter().find_map(|v| v.explanation);

        let locations = sorted.iter().map(|v| {
            let loc = match v.severity {
                Severity::Deny => format!("DENY  {}", v.key),
                Severity::Warn => v.key.clone(),
            };
            (loc, Some(v.message.clone()))
        });

        print_check_block(check, header_severity, &summary, description, locations);
    }

    if !diff.improvements.is_empty() {
        println!();
        let n = diff.improvements.len();
        let pad = RULE_BAR
            .chars()
            .count()
            .saturating_sub(28 + format!("{n} entries").chars().count());
        let dots = ".".repeat(pad.max(2));
        println!(
            "✨ {} {} {}",
            bold("ratchet improvements"),
            dim(&dots),
            green(&format!("{n} entries")),
        );
        println!("{}", dim(RULE_BAR));
        for imp in &diff.improvements {
            println!(
                "  {arrow} {check}/{key}: {from} → {to}",
                arrow = green("▸"),
                check = imp.check,
                key = imp.key,
                from = dim(&imp.from.to_string()),
                to = green(&imp.to.to_string()),
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
