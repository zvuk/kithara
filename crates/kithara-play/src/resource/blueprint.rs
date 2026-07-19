use std::fmt;

use kithara_audio::ResamplerBackend;
use kithara_decode::DecodeResult;
use kithara_events::EventBus;
use kithara_platform::{
    CancelScope, CancelToken,
    maybe_send::{BoxFuture, MaybeSend, MaybeSync},
    sync::Arc,
};

use super::{Resource, ResourceConfig};

type OpenFuture = BoxFuture<'static, DecodeResult<Resource>>;

trait OpenResource: MaybeSend + MaybeSync {
    fn open(&self, blueprint: ResourceBlueprint, event_bus: Option<EventBus>) -> OpenFuture;
}

struct ConfigBlueprint<B: Default> {
    cancel: CancelToken,
    config: ResourceConfig<B>,
}

impl<B> OpenResource for ConfigBlueprint<B>
where
    B: Default + ResamplerBackend,
{
    fn open(&self, blueprint: ResourceBlueprint, event_bus: Option<EventBus>) -> OpenFuture {
        let mut config = self.config.clone();
        config.cancel = Some(self.cancel.child());
        if let Some(event_bus) = event_bus {
            config.bus = Some(event_bus);
        }
        Box::pin(async move { Resource::open_config(config, blueprint).await })
    }
}

/// Immutable recipe for opening independent readers of one audio resource.
#[derive(Clone)]
pub struct ResourceBlueprint(Arc<dyn OpenResource>);

impl ResourceBlueprint {
    /// Creates a reusable resource recipe from a fully resolved configuration.
    #[must_use]
    pub fn new<B>(mut config: ResourceConfig<B>) -> Self
    where
        B: Default + ResamplerBackend,
    {
        let cancel = CancelScope::new(config.cancel.take()).token();
        Self(Arc::new(ConfigBlueprint { cancel, config }))
    }

    /// Opens an independent reader with its own decoder and cancellation child.
    pub async fn open(&self) -> DecodeResult<Resource> {
        self.0.open(self.clone(), None).await
    }

    /// Opens an internal reader whose decoder lifecycle does not publish into
    /// the application-facing resource event scope.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) async fn open_isolated(&self) -> DecodeResult<Resource> {
        self.0.open(self.clone(), Some(EventBus::default())).await
    }
}

impl fmt::Debug for ResourceBlueprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResourceBlueprint").finish_non_exhaustive()
    }
}
