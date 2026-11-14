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
}

impl TestBudget {
    /// Resolve the selected mode without consulting thread-local clock state.
    #[must_use]
    pub const fn timeout(self, flash: bool) -> Duration {
        let seconds = match self {
            Self::Quick => 1,
            Self::Short => 2,
            Self::Medium => 5,
            Self::Long => 10,
        };
        if cfg!(miri) {
            Duration::from_secs(match self {
                Self::Long => 480,
                _ => seconds * 60,
            })
        } else {
            Duration::from_secs(if flash { 1 } else { seconds })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Duration, TestBudget};
    use crate::kithara;

    #[kithara::test]
    fn budgets_are_available_in_constants() {
        const REAL: Duration = TestBudget::Medium.timeout(false);
        const FLASH: Duration = TestBudget::Medium.timeout(true);

        if cfg!(miri) {
            assert_eq!(REAL, Duration::from_secs(300));
            assert_eq!(FLASH, REAL);
        } else {
            assert_eq!(REAL, Duration::from_secs(5));
            assert_eq!(FLASH, Duration::from_secs(1));
        }
    }

    #[kithara::test]
    fn every_budget_keeps_its_mode_contract() {
        for (budget, real, miri) in [
            (TestBudget::Quick, 1, 60),
            (TestBudget::Short, 2, 120),
            (TestBudget::Medium, 5, 300),
            (TestBudget::Long, 10, 480),
        ] {
            let expected = if cfg!(miri) { miri } else { real };
            assert_eq!(budget.timeout(false), Duration::from_secs(expected));
            assert_eq!(
                budget.timeout(true),
                Duration::from_secs(if cfg!(miri) { miri } else { 1 }),
            );
        }
        assert_eq!(TestBudget::default(), TestBudget::Quick);
    }
}
