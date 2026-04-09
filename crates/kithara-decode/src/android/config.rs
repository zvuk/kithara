use std::sync::{Arc, atomic::AtomicU64};

use kithara_bufpool::PcmPool;
use kithara_stream::{ContainerFormat, StreamContext};

#[derive(Clone, Default)]
pub(crate) struct AndroidConfig {
    pub(crate) byte_len_handle: Option<Arc<AtomicU64>>,
    pub(crate) container: Option<ContainerFormat>,
    pub(crate) pcm_pool: Option<PcmPool>,
    pub(crate) stream_ctx: Option<Arc<dyn StreamContext>>,
    pub(crate) epoch: u64,
}
