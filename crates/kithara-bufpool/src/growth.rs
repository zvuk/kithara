use std::error::Error as StdError;

/// Error returned when controlled growth cannot reserve budget or capacity.
///
/// Returned by [`PooledOwned::ensure_len()`](crate::PooledOwned) when growing
/// a buffer would exceed the pool's configured `max_bytes` budget or the
/// allocator rejects its capacity request.
#[derive(Debug, Clone, derive_more::Display, PartialEq, Eq)]
#[display("byte budget exhausted")]
pub struct BudgetExhausted;

impl StdError for BudgetExhausted {}
