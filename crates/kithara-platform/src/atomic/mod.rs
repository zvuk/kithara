//! Fixed-order atomic values for independent scalar state.
//!
//! Use these for load/store-only fields. Counters, compare-and-swap state, and
//! fields requiring different orderings at different call sites use backend atomics.

mod order;
pub use order::{Acquire, ReadOrder, Relaxed, Release, SeqCst, WriteOrder};

mod value;
pub use value::{AtomicPrimitive, AtomicValue, RelaxedAtomicF32};
