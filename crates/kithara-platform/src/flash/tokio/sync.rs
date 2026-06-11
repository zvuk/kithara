//! Facade-shaped grouping of the sim-participating async channels: the
//! `crate::tokio::sync` facade re-exports [`mpsc`] and [`oneshot`] through this
//! path. The channel modules live as flat siblings (not a `sync/` directory)
//! to stay within the `max_nesting` arch limit.
pub use super::{mpsc, oneshot};
