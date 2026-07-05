use std::fmt;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum Severity {
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
pub struct Violation {
    pub check: &'static str,
    pub severity: Severity,
    pub key: String,
    pub message: String,
    /// Optional long-form explanation (Summary / Why / Bad / Good /
    /// Suppress block) shown in `--verbose` output and markdown reports.
    /// `None` keeps the compact one-line render.
    pub(crate) explanation: Option<&'static str>,
}

impl Violation {
    #[must_use]
    pub fn deny<K, M>(check: &'static str, key: K, message: M) -> Self
    where
        K: Into<String>,
        M: Into<String>,
    {
        build(Severity::Deny, check, key, message)
    }

    #[must_use]
    pub fn warn<K, M>(check: &'static str, key: K, message: M) -> Self
    where
        K: Into<String>,
        M: Into<String>,
    {
        build(Severity::Warn, check, key, message)
    }

    /// Attach the long-form Summary / Why / Bad / Good / Suppress block
    /// to this violation. Shown in `--verbose` and markdown report
    /// renderers; the compact one-line form keeps using `message`.
    #[must_use]
    pub fn with_explanation(mut self, explanation: &'static str) -> Self {
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
pub struct Report {
    pub violations: Vec<Violation>,
}

impl Report {
    #[must_use]
    pub fn deny_count(&self) -> usize {
        self.violations
            .iter()
            .filter(|v| v.severity == Severity::Deny)
            .count()
    }

    pub fn extend<I>(&mut self, vs: I)
    where
        I: IntoIterator<Item = Violation>,
    {
        self.violations.extend(vs);
    }

    #[must_use]
    pub fn warn_count(&self) -> usize {
        self.violations
            .iter()
            .filter(|v| v.severity == Severity::Warn)
            .count()
    }
}
