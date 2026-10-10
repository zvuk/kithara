#[cfg(feature = "lint")]
pub mod arch;
#[cfg(feature = "tools")]
pub mod ast_grep;
#[cfg(feature = "lint")]
pub mod audit;
#[cfg(feature = "tools")]
pub mod audit_clippy;
#[cfg(feature = "tools")]
pub mod ci_report;
#[cfg(feature = "tools")]
pub mod clippy;
#[cfg(feature = "tools")]
mod cohesion;
#[cfg(feature = "tools")]
mod command;
#[cfg(feature = "tools")]
pub mod common;
#[cfg(feature = "tools")]
pub mod ctx;
#[cfg(feature = "tools")]
pub mod format;
#[cfg(feature = "tools")]
pub mod health;
#[cfg(feature = "lint")]
pub mod idioms;
#[cfg(feature = "tools")]
pub mod init;
#[cfg(feature = "tools")]
pub mod junit;
#[cfg(feature = "tools")]
pub mod lease;
#[cfg(feature = "lint")]
pub mod lint;
#[cfg(feature = "tools")]
pub mod lock;
#[cfg(feature = "tools")]
pub mod manifest;
#[cfg(feature = "tools")]
pub mod orphans;
#[cfg(feature = "tools")]
pub mod perf;
#[cfg(feature = "tools")]
pub mod perf_compare;
#[cfg(feature = "tools")]
pub mod powerset;
#[cfg(feature = "tools")]
pub mod quality;
#[cfg(feature = "tools")]
pub mod quality_assessment;
#[cfg(feature = "tools")]
pub mod quality_lab;
#[cfg(feature = "tools")]
mod retried;
#[cfg(feature = "tools")]
pub mod sccache;
#[cfg(feature = "tools")]
pub mod scope;
#[cfg(feature = "tools")]
pub mod semver;
#[cfg(feature = "tools")]
pub mod similarity;
#[cfg(feature = "tools")]
mod stages;
#[cfg(feature = "tools")]
pub mod stress;
#[cfg(feature = "tools")]
mod stress_report;
#[cfg(feature = "tools")]
mod stress_run;
#[cfg(feature = "lint")]
pub mod style;
#[cfg(feature = "tools")]
pub mod test;
#[cfg(feature = "tools")]
mod touched;
#[cfg(feature = "tools")]
pub mod typos;
#[cfg(feature = "tools")]
pub mod util;
#[cfg(feature = "tools")]
pub mod verdict;
#[cfg(feature = "trace")]
pub mod viz;

#[cfg(feature = "tools")]
pub use command::{CoreCommand, run};
#[cfg(feature = "tools")]
pub use ctx::Ctx;
#[cfg(feature = "tools")]
mod consts;
