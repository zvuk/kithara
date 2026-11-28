use kithara_platform::time::Duration;

/// Wall-time allowance for a test in real, Flash, `RTSan` or Miri execution.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum TestBudget {
    #[default]
    Quick,
    Long,
    Heavy,
    /// A test against a live network or run only by hand, never a gate test.
    Network,
}

impl TestBudget {
    /// Resolve the wall-time allowance for the selected clock and compiled mode.
    #[must_use]
    pub const fn timeout(self, flash: bool) -> Duration {
        let (real, rtsan, miri) = match self {
            Self::Quick => (1, 1, 60),
            Self::Long => (30, 30, 480),
            Self::Heavy => (90, 90, 480),
            Self::Network => (600, 600, 480),
        };
        Duration::from_secs(if cfg!(miri) {
            miri
        } else if cfg!(rtsan) {
            rtsan
        } else if flash {
            1
        } else {
            real
        })
    }
}

/// Select the named allowance for the test's requested and compiled clock mode.
#[must_use]
pub const fn timeout_for_clock(real: Duration, flash: Duration, requested: bool) -> Duration {
    if requested && cfg!(feature = "flash") {
        flash
    } else {
        real
    }
}

#[cfg(test)]
mod tests {
    use super::{Duration, TestBudget, timeout_for_clock};
    use crate::kithara;

    #[kithara::test]
    fn budgets_are_available_in_constants() {
        const REAL: Duration = TestBudget::Long.timeout(false);
        const FLASH: Duration = TestBudget::Long.timeout(true);

        let real_expected = if cfg!(miri) { 480 } else { 30 };
        let flash_expected = if cfg!(miri) { 480 } else { 1 };
        assert_eq!(REAL, Duration::from_secs(real_expected));
        assert_eq!(FLASH, Duration::from_secs(flash_expected));
    }

    #[kithara::test]
    fn every_budget_keeps_its_mode_contract() {
        for (budget, real, rtsan, miri) in [
            (TestBudget::Quick, 1, 1, 60),
            (TestBudget::Long, 30, 30, 480),
            (TestBudget::Heavy, 90, 90, 480),
            (TestBudget::Network, 600, 600, 480),
        ] {
            let real_expected = if cfg!(miri) {
                miri
            } else if cfg!(rtsan) {
                rtsan
            } else {
                real
            };
            let flash_expected = if cfg!(miri) {
                miri
            } else if cfg!(rtsan) {
                rtsan
            } else {
                1
            };
            assert_eq!(budget.timeout(false), Duration::from_secs(real_expected));
            assert_eq!(budget.timeout(true), Duration::from_secs(flash_expected));
        }
        assert_eq!(TestBudget::default(), TestBudget::Quick);
    }

    #[kithara::test]
    fn clock_timeout_requires_requested_and_compiled_flash() {
        let real = TestBudget::Heavy.timeout(false);
        let flash = TestBudget::Quick.timeout(true);
        assert_eq!(
            timeout_for_clock(real, flash, true),
            if cfg!(feature = "flash") { flash } else { real }
        );
        assert_eq!(timeout_for_clock(real, flash, false), real);
    }
}
