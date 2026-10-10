use std::{marker::PhantomData, num::NonZeroU32};

use kithara_bufpool::HasPool;
use kithara_command::{Live, ScopedConfig};
use kithara_platform::{maybe_send::MaybeSend, sync::Arc};
use kithara_play::PlayError;

use super::PlatformResult;
#[cfg(feature = "offline")]
use crate::host::{
    HostConfig,
    offline::{OfflineRuntime, StartedOffline},
};
use crate::{
    HostCore, HostOwner, HostSettings,
    rt::SessionOutput,
    session::{HostDispatcher, HostProtocol, HostRoot, RootView},
};

type StartedPlatform<S, O> = (
    Arc<dyn HostDispatcher<<O as HostOwner<S>>::Command>>,
    Platform<S, O>,
);

type PlatformMarker<S, O> = PhantomData<fn() -> (S, O)>;

impl<S, O: HostOwner<S>> PlatformResult<Self> for StartedPlatform<S, O> {
    fn resolve(self) -> Result<Self, PlayError> {
        Ok(self)
    }
}

pub(in crate::host) struct Platform<S, O: HostOwner<S>> {
    marker: PlatformMarker<S, O>,
}

impl<S, O: HostOwner<S>> Platform<S, O> {
    #[cfg(feature = "offline")]
    pub(in crate::host) fn offline(
        config: HostConfig<S>,
        root: HostRoot,
        view: RootView,
        layer: impl FnOnce(HostCore<S, O::Deck>) -> O + MaybeSend + 'static,
    ) -> Result<StartedOffline<S, O>, PlayError>
    where
        S: HasPool<f32> + Send + Sync + 'static,
    {
        let (dispatcher, runtime) = OfflineRuntime::new(config, root, view, layer)?;
        Ok((
            dispatcher,
            Self {
                marker: PhantomData,
            },
            runtime,
        ))
    }

    pub(in crate::host) fn realtime(
        root: HostRoot,
        view: RootView,
        output_block_frames: Option<NonZeroU32>,
        channel_config: ScopedConfig,
        output: SessionOutput,
        settings: Live<HostSettings, HostProtocol>,
        layer: impl FnOnce(HostCore<S, O::Deck>) -> O + MaybeSend + 'static,
    ) -> StartedPlatform<S, O>
    where
        S: HasPool<f32> + Send + Sync + 'static,
    {
        let session = crate::session::native_engine::spawn::<S, O>(
            root,
            view,
            output_block_frames,
            channel_config,
            output,
            settings,
            layer,
        );
        (
            session,
            Self {
                marker: PhantomData,
            },
        )
    }
}
