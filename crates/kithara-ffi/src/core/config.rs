/// Store configuration forwarded from platform layer to resource creation.
#[derive(Clone, Debug, Default)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct StoreOptions {
    pub cache_dir: Option<String>,
}
