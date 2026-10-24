use std::sync::{LazyLock, OnceLock};

use kithara::{
    host::{HostConfig, wasm},
    platform::{
        sync::{Arc, Mutex, mpsc},
        thread,
    },
    play::{ResourcePrep, wasm as play_wasm},
    queue::{QueueCommand, QueueConfig, QueueError, TrackId},
};
use kithara_command::Ticket;
use wasm_bindgen::JsValue;

use crate::{
    pools::{
        FfiHost, FfiPools, FfiQueue, FfiQueueControl, FfiTrackSource, Pools, build as build_pools,
    },
    types::{FfiError, FfiKeyRule},
    web::{analysis::AnalysisRequest, resources::ResourceBuilder},
};

struct HostChannel {
    _host: FfiHost,
    receiver: wasm::HostReceiver<FfiPools>,
    sender: wasm::HostSender<FfiPools>,
    pools: Pools,
}

fn host_channel() -> &'static Mutex<Option<HostChannel>> {
    static CHANNEL: LazyLock<Mutex<Option<HostChannel>>> = LazyLock::new(|| Mutex::new(None));
    &CHANNEL
}

fn ensure_host_channel() -> Result<(wasm::HostSender<FfiPools>, Pools), JsValue> {
    let mut guard = host_channel().lock();
    if let Some(channel) = guard.as_ref() {
        return Ok((channel.sender.clone(), channel.pools.clone()));
    }

    let pools = build_pools()
        .map_err(|error| JsValue::from_str(&format!("pool construction failed: {error}")))?;
    let host = FfiHost::new(HostConfig::builder().build())
        .map_err(|error| JsValue::from_str(&format!("host construction failed: {error}")))?;
    let (sender, receiver) = wasm::worker_host_channel(&host)
        .map_err(|error| JsValue::from_str(&format!("host channel failed: {error}")))?;
    play_wasm::spawn_webcodecs_probe(pools.clone());
    *guard = Some(HostChannel {
        receiver,
        _host: host,
        pools: pools.clone(),
        sender: sender.clone(),
    });
    Ok((sender, pools))
}

pub(crate) fn initialize() -> Result<(), JsValue> {
    ensure_host_channel().map(drop)
}

pub(crate) fn tick_and_poll() {
    let guard = host_channel().lock();
    if let Some(channel) = guard.as_ref() {
        wasm::tick_and_poll(&channel.receiver);
    }
}

struct Engine {
    queue: FfiQueueControl,
    resources: Mutex<ResourceBuilder>,
    analysis: mpsc::Sender<AnalysisRequest>,
}

/// Main-thread endpoint to the Queue's sole postbox and published view.
#[derive(Default)]
pub(crate) struct QueueBridge {
    pub(in crate::web) queue: Arc<OnceLock<FfiQueueControl>>,
    engine: OnceLock<Engine>,
}

impl QueueBridge {
    fn engine(&self) -> Result<&Engine, FfiError> {
        thread::assert_main_thread(concat!(module_path!(), "::engine"));
        if let Some(engine) = self.engine.get() {
            return Ok(engine);
        }
        let (host_sender, pools) = ensure_host_channel().map_err(|error| FfiError::Internal {
            description: error
                .as_string()
                .unwrap_or_else(|| "host construction failed".into()),
        })?;
        let resources = ResourceBuilder::new(pools.clone());
        let queue = FfiQueue::new(
            QueueConfig::builder()
                .prep(
                    ResourcePrep::builder()
                        .worker(resources.worker.clone())
                        .build(),
                )
                .settings(crate::web::worker::queue_settings())
                .store(resources.store.clone())
                .downloader(resources.downloader.clone())
                .build(),
        );
        let control = queue.control();
        let (analysis, requests) = mpsc::channel();
        let worker = thread::spawn(move || {
            crate::web::worker::worker_main(queue, host_sender, pools, requests);
        });
        std::mem::forget(worker);
        self.queue.get_or_init(|| control.clone());
        Ok(self.engine.get_or_init(|| Engine {
            queue: control,
            resources: Mutex::new(resources),
            analysis,
        }))
    }

    pub(in crate::web) fn queue(&self) -> Option<&FfiQueueControl> {
        self.queue.get()
    }

    pub(in crate::web) fn post(
        &self,
        command: QueueCommand<FfiPools>,
    ) -> Result<Ticket<QueueError>, FfiError> {
        self.engine()?
            .queue
            .post(command)
            .map_err(|error| FfiError::Internal {
                description: error.to_string(),
            })
    }

    delegate::delegate! {
        to self {
            #[expr($.map_or(0, FfiQueueControl::mixer_blocks))]
            #[call(queue)]
            pub(crate) fn process_calls(&self) -> u64;
            #[expr($.map_or(0, |queue| queue.mixer_metrics().underruns()))]
            #[call(queue)]
            pub(crate) fn underruns(&self) -> u64;
            #[expr(Ok($?.queue.clone()))]
            #[call(engine)]
            pub(in crate::web) fn control(&self) -> Result<FfiQueueControl, FfiError>;
            #[expr($?.current().map(|entry| entry.id))]
            #[call(queue)]
            pub(crate) fn current_track_id(&self) -> Option<TrackId>;
        }
    }

    pub(in crate::web) fn source(&self, url: String) -> Result<FfiTrackSource, FfiError> {
        self.engine()?
            .resources
            .lock()
            .source(url)
            .map_err(|reason| FfiError::InvalidArgument { reason })
    }

    pub(in crate::web) fn analysis_request(
        &self,
        id: TrackId,
    ) -> Result<(mpsc::Sender<AnalysisRequest>, AnalysisRequest), FfiError> {
        let engine = self.engine()?;
        Ok((
            engine.analysis.clone(),
            AnalysisRequest {
                id,
                request_id: crate::web::interop::next_request_id(),
                resources: engine.resources.lock().clone(),
            },
        ))
    }

    pub(in crate::web) fn set_auth_token(&self, token: String) -> Result<(), FfiError> {
        self.engine()?.resources.lock().set_auth_token(token);
        Ok(())
    }

    pub(in crate::web) fn register_key_rule(
        &self,
        rule: FfiKeyRule,
        salt: String,
    ) -> Result<(), FfiError> {
        self.engine()?
            .resources
            .lock()
            .register_key_rule(rule, salt);
        Ok(())
    }

    pub(crate) fn position_secs(&self) -> f64 {
        self.queue()
            .and_then(FfiQueueControl::position_seconds)
            .unwrap_or(0.0)
    }
}
