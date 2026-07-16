mod contract;
mod default;
mod registry;
mod resource;
mod scope;
mod source;
mod validation;

pub use contract::AssetLayout;
pub use default::DefaultLayout;
pub use registry::AssetLayoutRegistry;
pub(crate) use resource::ResourceKeyKind;
pub use resource::{AssetResource, ResourceKey};
pub use scope::AssetScope;
pub use source::AssetSource;
pub(crate) use source::{local_root, remote_root};
pub(crate) use validation::{validate_path, validate_root, validate_source};
