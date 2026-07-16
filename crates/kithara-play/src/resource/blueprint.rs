use std::{fmt, future::Future, pin::Pin};

use kithara_audio::ResamplerBackend;
use kithara_decode::DecodeResult;
#[cfg(not(target_arch = "wasm32"))]
use kithara_events::EventBus;
use kithara_platform::{CancelScope, CancelToken, sync::Arc};

use super::{Resource, ResourceConfig};

#[cfg(not(target_arch = "wasm32"))]
type OpenFuture = Pin<Box<dyn Future<Output = DecodeResult<Resource>> + Send + 'static>>;

#[cfg(target_arch = "wasm32")]
type OpenFuture = Pin<Box<dyn Future<Output = DecodeResult<Resource>> + 'static>>;

#[derive(Clone, Copy)]
enum EventScope {
    Configured,
    #[cfg(not(target_arch = "wasm32"))]
    Isolated,
}

trait OpenResource: Send + Sync {
    fn open(&self, blueprint: ResourceBlueprint, event_scope: EventScope) -> OpenFuture;
}

struct ConfigBlueprint<B: Default> {
    cancel: CancelToken,
    config: ResourceConfig<B>,
}

impl<B> OpenResource for ConfigBlueprint<B>
where
    B: Default + ResamplerBackend,
{
    fn open(&self, blueprint: ResourceBlueprint, event_scope: EventScope) -> OpenFuture {
        let mut config = self.config.clone();
        config.cancel = Some(self.cancel.child());
        #[cfg(target_arch = "wasm32")]
        let _ = event_scope;
        #[cfg(not(target_arch = "wasm32"))]
        if matches!(event_scope, EventScope::Isolated) {
            config.bus = Some(EventBus::default());
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
        self.0.open(self.clone(), EventScope::Configured).await
    }

    /// Opens an internal reader whose decoder lifecycle does not publish into
    /// the application-facing resource event scope.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) async fn open_isolated(&self) -> DecodeResult<Resource> {
        self.0.open(self.clone(), EventScope::Isolated).await
    }
}

impl fmt::Debug for ResourceBlueprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResourceBlueprint").finish_non_exhaustive()
    }
}
