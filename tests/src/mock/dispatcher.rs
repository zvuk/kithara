use std::fmt;

use kithara::{
    audio::{Audio, ResamplerBackend},
    bufpool::HasPool,
    events::EventBus,
    platform::{
        sync::{Arc, Mutex},
        time::Duration,
    },
    play::{PlayWorker, TrackConfig},
    stream::{Stream, StreamType},
};
use kithara_command::{ChannelConfig, Inbox, Sender, channel};
use kithara_render::{
    DecoderNode, DispatcherProtocol, LaneProtocol, LaneStart, LoadRefusal, Open, OpenResult,
    PcmReceiver,
};

#[cfg(not(target_arch = "wasm32"))]
pub(super) type ReleaseLane = Box<dyn FnMut() + Send>;
#[cfg(target_arch = "wasm32")]
pub(super) type ReleaseLane = Box<dyn FnMut()>;

/// One test-side dispatcher owner for readers sharing a bare playback worker.
pub struct LaneLoader<T, B, S>
where
    T: StreamType<Events = EventBus>,
    B: Default + ResamplerBackend,
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    pub(super) worker: PlayWorker<S>,
    pub(super) owner: Arc<Mutex<LoadOwner<T, B, S>>>,
}

pub(super) struct LoadOwner<T, B, S>
where
    T: StreamType<Events = EventBus>,
    B: Default + ResamplerBackend,
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    pub(super) sender: Sender<DispatcherProtocol<LaneOpen<T, B, S>>>,
    _dispatcher: kithara::worker::TaskHandle,
}

impl<T, B, S> LaneLoader<T, B, S>
where
    T: StreamType<Events = EventBus>,
    B: Default + ResamplerBackend,
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    pub fn new(worker: &PlayWorker<S>) -> Result<Self, LoadRefusal> {
        let (sender, inbox) = channel(ChannelConfig::builder().build());
        let dispatcher = worker.start_dispatcher(inbox)?;
        Ok(Self {
            worker: worker.clone(),
            owner: Arc::new(Mutex::new(LoadOwner {
                sender,
                _dispatcher: dispatcher,
            })),
        })
    }
}

pub(super) struct LaneOpen<T: StreamType, B: ResamplerBackend, S> {
    pub(super) worker: PlayWorker<S>,
    pub(super) config: TrackConfig<T, B>,
}

impl<T: StreamType, B: ResamplerBackend, S> fmt::Debug for LaneOpen<T, B, S> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("LaneOpen").finish_non_exhaustive()
    }
}

impl<T, B, S> Open for LaneOpen<T, B, S>
where
    T: StreamType<Events = EventBus>,
    B: Default + ResamplerBackend,
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    type Opened = PcmReceiver;
    type Lane = DecoderNode<Audio<Stream<T>>, S>;

    async fn open(
        self,
        position: Duration,
        start: LaneStart,
        inbox: Inbox<LaneProtocol>,
    ) -> OpenResult<Self::Opened, Self::Lane> {
        self.worker.load(self.config, position, start, inbox).await
    }
}
