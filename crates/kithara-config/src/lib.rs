//! Owned snapshots of retained settings, with builders and accessors.

pub use bon;
/// `#[derive(Config)]` declares builders, accessors, defaults, debug output,
/// snapshots, checks and live changes through `#[config(...)]`. `construction`
/// consumes unmarked builder inputs without a snapshot. `fields(...)` supplies
/// field defaults; explicit facets override them and groups replace whole groups.
/// `get(ref)` borrows the field; `get(skip)` disables an inherited getter.
///
/// ```compile_fail
/// #[derive(kithara_config::Config)]
/// struct Unclassified { value: u32 }
/// ```
pub use kithara_derive::Config;
pub use kithara_derive::{ConfigOwner, Patch};

mod config;
pub mod duration;
mod env;
mod live;
pub use config::{Config, ConfigOwner};
pub use env::path_from_env;
pub use live::{CheckedConfig, Configure, LiveConfig, Nested};

#[doc(hidden)]
pub mod __private;
