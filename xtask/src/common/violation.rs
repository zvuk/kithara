//! Common types for emitted check results.

use std::fmt;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) enum Severity {
    Warn,
    Deny,
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Warn => f.write_str("WARN"),
            Self::Deny => f.write_str("DENY"),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Violation {
    pub(crate) check: &'static str,
    pub(crate) severity: Severity,
    pub(crate) key: String,
    pub(crate) message: String,
    /// Optional long-form explanation (Summary / Why / Bad / Good /
    /// Suppress block) shown in `--verbose` output and markdown reports.
    /// `None` keeps the compact one-line render.
    pub(crate) explanation: Option<&'static str>,
}

impl Violation {
    pub(crate) fn deny(
        check: &'static str,
        key: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            check,
            severity: Severity::Deny,
            key: key.into(),
            message: message.into(),
            explanation: None,
        }
    }

    pub(crate) fn warn(
        check: &'static str,
        key: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            check,
            severity: Severity::Warn,
            key: key.into(),
            message: message.into(),
            explanation: None,
        }
    }

    /// Attach the long-form Summary / Why / Bad / Good / Suppress block
    /// to this violation. Shown in `--verbose` and markdown report
    /// renderers; the compact one-line form keeps using `message`.
    #[must_use]
    pub(crate) fn with_explanation(mut self, explanation: &'static str) -> Self {
        self.explanation = Some(explanation);
        self
    }
}

#[derive(Default)]
pub(crate) struct Report {
    pub(crate) violations: Vec<Violation>,
}

impl Report {
    pub(crate) fn extend(&mut self, vs: impl IntoIterator<Item = Violation>) {
        self.violations.extend(vs);
    }

    pub(crate) fn deny_count(&self) -> usize {
        self.violations
            .iter()
            .filter(|v| v.severity == Severity::Deny)
            .count()
    }

    pub(crate) fn warn_count(&self) -> usize {
        self.violations
            .iter()
            .filter(|v| v.severity == Severity::Warn)
            .count()
    }
}
