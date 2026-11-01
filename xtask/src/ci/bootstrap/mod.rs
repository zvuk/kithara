//! Cache and target-lease ownership before the full xtask is available.

mod cache;
mod command;
mod consts;
mod lease;
mod stats;

pub use cache::{CompilerCache, require_remote};
pub use command::run;
pub use stats::statistics;
