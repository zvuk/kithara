mod cli;
mod gecko;
mod junit;
mod lanes;
mod list;
mod matrix;
mod profile;
mod report;
mod slow;
mod trace;

pub use cli::PerfArgs;
pub(crate) use cli::run;
