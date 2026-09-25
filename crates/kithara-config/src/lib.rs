//! Owned snapshots of retained settings, with builders and accessors.

pub use kithara_derive::Patch;
/// Composes builders, accessors, snapshots and explicitly selected runtime updates.
/// `construction` classifies a consumed builder input without generating a retained snapshot.
/// On an owner method, `delegate = "property", sdk` registers an SDK operation
/// while preserving the method as its sole application hook.
///
/// ```compile_fail
/// #[kithara_config::config]
/// struct Unclassified { value: u32 }
/// ```
pub use kithara_derive::config;

mod config;
pub use config::Config;

mod live;
pub use live::{LiveBool, LiveF32};

#[doc(hidden)]
pub mod __private;
