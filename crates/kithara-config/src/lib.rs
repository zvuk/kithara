//! Owned snapshots of retained settings, with builders and accessors.

pub use bon;
/// `#[derive(Config)]` generates builders, getters, defaults, Debug, snapshots,
/// checks and live changes. `construction` consumes unmarked inputs without a snapshot.
/// `fields(...)` supplies defaults; facets override them and groups replace groups.
/// `get(ref)` borrows a field; `get(skip)` disables its inherited getter.
/// Nested live types name their enum through the same path with a `Change`
/// suffix; import that enum beside the configuration type.
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
