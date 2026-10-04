//! Owned snapshots of retained settings, with builders and accessors.

pub use bon;
/// `#[derive(Config)]` generates the builder, accessors, `Default`, `Debug`,
/// the owned snapshot, field checks and live field changes of a struct, every
/// facet declared through `#[config(...)]`. `construction` treats unmarked
/// fields as consumed builder inputs and generates no retained snapshot. On a struct
/// `fields(...)` supplies field defaults using the same grammar as a field
/// declaration, for example `fields(value, get(copy), builder(default))`.
/// `get(ref)` borrows the retained field; `get(skip)` disables an inherited
/// getter. Explicit field facets override defaults; groups replace whole groups.
///
/// ```compile_fail
/// #[derive(kithara_config::Config)]
/// struct Unclassified { value: u32 }
/// ```
pub use kithara_derive::Config;
pub use kithara_derive::{ConfigOwner, Patch};

mod config;
mod live;
pub use config::{Config, ConfigOwner, ConfigOwnerMut};
pub use live::{CheckedConfig, Configure, LiveConfig, Nested};

#[doc(hidden)]
pub mod __private;
