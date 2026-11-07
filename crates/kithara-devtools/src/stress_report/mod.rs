//! Builds a bounded Markdown summary from a nextest stress `JUnit` report.

mod blocker;
mod evidence;
mod sanitizer;
mod summary;

pub(crate) use evidence::parse_timestamp_ms;
pub(crate) use sanitizer::{Findings, findings as sanitizer_findings};
pub(crate) use summary::{
    Comparison, LaneRate, LaneReport, StressReportArgs, append_attempt_reports, attempt_records,
    lane_report, rate_percent, read_bounded_utf8, read_primary_evidence, render_run_comparison,
    run, validate_inventory, validate_primary_evidence, write_report,
};
use summary::{markdown_cell, test_id};

pub(crate) use crate::test::TestId;
