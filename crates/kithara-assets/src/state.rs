#![forbid(unsafe_code)]

use kithara_storage::ResourceStatus;

/// Side-effect-free view of a resource in an asset store.
///
/// Unlike `open_resource()`, querying this state must not create, reopen, or
/// mutate the underlying resource.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum AssetResourceState {
    /// No resource is currently present for this key.
    Missing,
    /// A live resource exists and is still writable.
    Active,
    /// A committed resource exists and can be read as-is.
    Committed { final_len: Option<u64> },
    /// A live resource exists but has already failed.
    Failed(String),
}

impl From<ResourceStatus> for AssetResourceState {
    fn from(status: ResourceStatus) -> Self {
        match status {
            ResourceStatus::Active => Self::Active,
            ResourceStatus::Committed { final_len } => Self::Committed { final_len },
            ResourceStatus::Failed(reason) => Self::Failed(reason),
        }
    }
}
