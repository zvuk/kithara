use crate::layout::FfiLayout;

/// Store configuration forwarded from platform layer to resource creation.
#[derive(Clone, Debug, Default)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct StoreOptions {
    pub cache_dir: Option<String>,
    /// On-disk cache layout. `None` == [`FfiLayout::Default`].
    #[cfg_attr(feature = "uniffi", uniffi(default = None))]
    pub layout: Option<FfiLayout>,
}
