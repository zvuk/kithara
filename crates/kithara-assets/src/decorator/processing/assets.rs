use std::path::Path;

use kithara_bufpool::HasPool;
use kithara_platform::sync::Arc;

use super::{contract::ProcessCtx, reader::ProcessedReader, writer::ProcessedWriter};
use crate::{
    decorator::{Assets, Capabilities},
    error::AssetsResult,
    layout::ResourceKey,
    resource::{AcquisitionResult, AssetResourceState, RequestIdentity},
    store::{AssetStoreConfig, AssetStoreConfigOwnerAccess},
};

/// Applies optional resource processing to another asset store.
#[derive_where::derive_where(Clone; A: Assets)]
#[derive(kithara_config::ConfigOwner)]
#[config_owner(AssetStoreConfig<S>, config)]
pub struct ProcessingAssets<A, S>
where
    A: Assets,
    S: HasPool<u8> + Send + Sync + 'static,
{
    inner: Arc<A>,
    config: Arc<AssetStoreConfig<S>>,
}

impl<A, S> ProcessingAssets<A, S>
where
    A: Assets,
    S: HasPool<u8> + Send + Sync + 'static,
{
    pub fn new(inner: Arc<A>, config: Arc<AssetStoreConfig<S>>) -> Self {
        Self { inner, config }
    }

    fn wrap_ready(
        &self,
        inner: A::ReadyRes,
        processor: Option<ProcessCtx>,
    ) -> ProcessedReader<A::ReadyRes, S> {
        ProcessedReader::wrap_ready()
            .inner(inner)
            .maybe_processor(processor)
            .pools(self.config.pools.clone())
            .maybe_chunk_size(self.processing_chunk_size())
            .maybe_gate_poll_interval(self.processing_gate_poll_interval())
            .call()
    }
}

impl<A, S> Assets for ProcessingAssets<A, S>
where
    A: Assets,
    S: HasPool<u8> + Send + Sync + 'static,
{
    type ActiveRes = ProcessedWriter<A::ActiveRes, S>;
    type Context = ProcessCtx;
    type ReadyRes = ProcessedReader<A::ReadyRes, S>;

    fn acquire_resource_with_ctx(
        &self,
        key: &ResourceKey,
        identity: Option<&RequestIdentity>,
        ctx: Option<Self::Context>,
    ) -> AssetsResult<AcquisitionResult<Self::ActiveRes, Self::ReadyRes>> {
        match self.inner.acquire_resource(key, identity)? {
            AcquisitionResult::Pending(writer) => Ok(AcquisitionResult::Pending(
                ProcessedWriter::builder()
                    .inner(writer)
                    .maybe_processor(ctx)
                    .pools(self.config.pools.clone())
                    .maybe_chunk_size(self.processing_chunk_size())
                    .maybe_gate_poll_interval(self.processing_gate_poll_interval())
                    .build(),
            )),
            AcquisitionResult::Ready(reader) => {
                Ok(AcquisitionResult::Ready(self.wrap_ready(reader, ctx)))
            }
            AcquisitionResult::Joined(reader) => {
                Ok(AcquisitionResult::Joined(self.wrap_ready(reader, ctx)))
            }
        }
    }

    fn open_resource_with_ctx(
        &self,
        key: &ResourceKey,
        identity: Option<&RequestIdentity>,
        ctx: Option<Self::Context>,
    ) -> AssetsResult<Self::ReadyRes> {
        Ok(self.wrap_ready(self.inner.open_resource(key, identity)?, ctx))
    }

    delegate::delegate! {
        to self.inner {
            fn capabilities(&self) -> Capabilities;
            fn root_dir(&self) -> &Path;
            fn resource_state(&self, key: &ResourceKey) -> AssetsResult<AssetResourceState>;
            fn delete_asset(&self, asset_root: &str) -> AssetsResult<()>;
            fn remove_resource(&self, key: &ResourceKey) -> AssetsResult<()>;
        }
    }
}
