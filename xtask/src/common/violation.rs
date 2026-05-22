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
        build(Severity::Deny, check, key, message)
    }

    pub(crate) fn warn(
        check: &'static str,
        key: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        build(Severity::Warn, check, key, message)
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

/// Common builder used by both `Violation::deny` and `Violation::warn` to keep
/// their bodies DRY without exposing a third public-facing constructor on
/// `impl Violation` (the `multi_constructor` arch lint allows only one
/// canonical `new`/`default`).
fn build(
    severity: Severity,
    check: &'static str,
    key: impl Into<String>,
    message: impl Into<String>,
) -> Violation {
    Violation {
        check,
        severity,
        key: key.into(),
        message: message.into(),
        explanation: None,
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
