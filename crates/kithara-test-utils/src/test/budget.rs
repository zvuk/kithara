use kithara_platform::time::Duration;

/// Wall-time allowance for a test in real, Flash or Miri execution.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum TestBudget {
    #[default]
    Quick,
    Short,
    Medium,
    Long,
    Heavy,
    /// A test against a live network or run only by hand, never a gate test.
    Network,
}

impl TestBudget {
    /// Resolve the wall-time allowance for the compiled execution mode.
    #[must_use]
    pub const fn timeout(self) -> Duration {
        let seconds = match self {
            Self::Quick => 1,
            Self::Short => 2,
            Self::Medium => 5,
            Self::Long => 10,
            Self::Heavy => 90,
            Self::Network => 600,
        };
        if cfg!(miri) {
            Duration::from_secs(match self {
                Self::Long | Self::Heavy | Self::Network => 480,
                _ => seconds * 60,
            })
        } else {
            Duration::from_secs(seconds)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Duration, TestBudget};
    use crate::kithara;

    #[kithara::test]
    fn budgets_are_available_in_constants() {
        const MEDIUM: Duration = TestBudget::Medium.timeout();

        let expected = if cfg!(miri) { 300 } else { 5 };
        assert_eq!(MEDIUM, Duration::from_secs(expected));
    }

    #[kithara::test]
    fn every_budget_keeps_its_mode_contract() {
        for (budget, real, miri) in [
            (TestBudget::Quick, 1, 60),
            (TestBudget::Short, 2, 120),
            (TestBudget::Medium, 5, 300),
            (TestBudget::Long, 10, 480),
            (TestBudget::Heavy, 90, 480),
            (TestBudget::Network, 600, 480),
        ] {
            let expected = if cfg!(miri) { miri } else { real };
            assert_eq!(budget.timeout(), Duration::from_secs(expected));
        }
        assert_eq!(TestBudget::default(), TestBudget::Quick);
    }
}
