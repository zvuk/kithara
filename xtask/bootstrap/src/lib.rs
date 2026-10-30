//! Cache and target-lease ownership before the full xtask is available.

mod cache;
mod command;
mod consts;
mod lease;

pub use cache::{CompilerCache, require_remote};
pub use command::run;
