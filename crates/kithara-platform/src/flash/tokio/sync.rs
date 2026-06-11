//! Facade-shaped grouping of the async channel surface: the real
//! `tokio_with_wasm::alias::sync` items plus the sim-participating [`mpsc`]
//! and [`oneshot`] shadowing the glob. The channel modules live as flat
//! siblings (not a `sync/` directory) to stay within the `max_nesting` arch
//! limit.
pub use tokio_with_wasm::alias::sync::*;

pub use super::{mpsc, oneshot};
