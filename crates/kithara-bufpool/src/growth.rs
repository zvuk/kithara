use std::error::Error as StdError;

/// Error returned when a byte budget limit is exceeded.
///
/// Returned by [`PooledOwned::ensure_len()`](crate::PooledOwned) when growing
/// a buffer would exceed the pool's configured `max_bytes` budget.
#[derive(Debug, Clone, derive_more::Display, PartialEq, Eq)]
#[display("byte budget exhausted")]
pub struct BudgetExhausted;

impl StdError for BudgetExhausted {}
