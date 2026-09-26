#[cfg(feature = "lint")]
pub mod arch;
pub mod ast_grep;
#[cfg(feature = "lint")]
pub mod audit;
pub mod audit_clippy;
pub mod ci_report;
pub mod clippy;
mod cohesion;
mod command;
pub mod common;
pub mod ctx;
pub mod format;
pub mod health;
#[cfg(feature = "lint")]
pub mod idioms;
pub mod init;
pub mod junit;
pub mod lease;
#[cfg(feature = "lint")]
pub mod lint;
pub mod lock;
pub mod manifest;
pub mod orphans;
pub mod perf;
pub mod perf_compare;
pub mod powerset;
pub mod quality;
pub mod quality_assessment;
pub mod quality_lab;
mod retried;
pub mod sccache;
pub mod scope;
pub mod semver;
pub mod similarity;
mod stages;
pub mod stress;
mod stress_report;
mod stress_run;
#[cfg(feature = "lint")]
pub mod style;
pub mod test;
mod touched;
pub mod typos;
pub mod util;
pub mod verdict;
#[cfg(feature = "viz")]
pub mod viz;

pub use command::{CoreCommand, run};
pub use ctx::Ctx;
mod consts;
