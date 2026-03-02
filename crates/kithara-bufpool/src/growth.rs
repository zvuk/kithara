/// Error returned when a byte budget limit is exceeded.
///
/// Returned by [`PooledOwned::ensure_len()`](crate::PooledOwned) when growing
/// a buffer would exceed the pool's configured `max_bytes` budget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BudgetExhausted;

impl std::fmt::Display for BudgetExhausted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("byte budget exhausted")
    }
}

impl std::error::Error for BudgetExhausted {}
