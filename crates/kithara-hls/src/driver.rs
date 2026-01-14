use std::{pin::Pin, sync::Arc};

use async_stream::stream;
use bytes::Bytes;
use futures::{Stream, StreamExt};
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::{
    HlsError, HlsOptions, HlsResult,
    abr::{AbrConfig, AbrController},
    events::{EventEmitter, HlsEvent},
    fetch::FetchManager,
    keys::KeyManager,
    pipeline::{BaseStream, PipelineError, PipelineEvent, PipelineStream},
    playlist::PlaylistManager,
};

#[derive(Debug, thiserror::Error)]
pub enum DriverError {
    #[error("pipeline error: {0}")]
    Pipeline(#[from] PipelineError),
}

pub struct HlsDriver {
    options: HlsOptions,
    master_url: Url,
    playlist_manager: Arc<PlaylistManager>,
    fetch_manager: Arc<FetchManager>,
    key_manager: Arc<KeyManager>,
    event_emitter: Arc<EventEmitter>,
    cancel: CancellationToken,
}

impl HlsDriver {
    pub fn new(
        master_url: Url,
        options: HlsOptions,
        playlist_manager: Arc<PlaylistManager>,
        fetch_manager: Arc<FetchManager>,
        key_manager: Arc<KeyManager>,
        event_emitter: EventEmitter,
    ) -> Self {
        Self {
            options,
            master_url,
            playlist_manager,
            fetch_manager,
            key_manager,
            event_emitter: Arc::new(event_emitter),
            cancel: CancellationToken::new(),
        }
    }

    pub fn events(&self) -> broadcast::Receiver<HlsEvent> {
        self.event_emitter.subscribe()
    }

    pub fn stream(&self) -> Pin<Box<dyn Stream<Item = HlsResult<Bytes>> + Send + '_>> {
        let playlist_manager = Arc::clone(&self.playlist_manager);
        let fetch_manager = Arc::clone(&self.fetch_manager);
        let key_manager = Arc::clone(&self.key_manager);
        let events = Arc::clone(&self.event_emitter);
        let cancel = self.cancel.clone();
        let opts = self.options.clone();
        let master_url = self.master_url.clone();

        Box::pin(stream! {
            let abr_config = AbrConfig {
                min_buffer_for_up_switch_secs: opts.abr_min_buffer_for_up_switch as f64,
                down_switch_buffer_secs: opts.abr_down_switch_buffer as f64,
                throughput_safety_factor: opts.abr_throughput_safety_factor as f64,
                up_hysteresis_ratio: opts.abr_up_hysteresis_ratio as f64,
                down_hysteresis_ratio: opts.abr_down_switch_buffer as f64,
                min_switch_interval: opts.abr_min_switch_interval,
                initial_variant_index: opts.abr_initial_variant_index,
                ..AbrConfig::default()
            };

            let abr_controller =
                AbrController::new(abr_config, opts.variant_stream_selector.clone());

            let base = BaseStream::new(
                master_url,
                fetch_manager,
                playlist_manager,
                Some(key_manager),
                abr_controller,
                cancel.clone(),
            );

            // Prefetch with mpsc backpressure.
            let prefetch_capacity = opts.prefetch_buffer_size.unwrap_or(1).max(1);
            let (tx, mut rx) = mpsc::channel(prefetch_capacity);

            // Single background task: reads from BaseStream + forwards events.
            let mut ev_rx = base.event_sender().subscribe();
            let events_clone = Arc::clone(&events);
            let cancel_clone = cancel.clone();
            tokio::spawn(async move {
                let mut stream = Box::pin(base);
                loop {
                    tokio::select! {
                        biased;
                        ev = ev_rx.recv() => {
                            if let Ok(ev) = ev {
                                match ev {
                                    PipelineEvent::VariantApplied { from, to, reason } => {
                                        events_clone.emit_variant_applied(from, to, reason);
                                    }
                                    PipelineEvent::SegmentReady { variant, segment_index } => {
                                        events_clone.emit_segment_start(variant, segment_index, 0);
                                    }
                                    PipelineEvent::Decrypted { .. } | PipelineEvent::Prefetched { .. } => {}
                                }
                            }
                        }
                        item = stream.next() => {
                            let Some(item) = item else { break };
                            if cancel_clone.is_cancelled() { break }
                            if tx.send(item).await.is_err() { break }
                        }
                    }
                }
            });

            // Consumer yields items.
            while let Some(item) = rx.recv().await {
                match item {
                    Ok(payload) => {
                        yield Ok(payload.bytes);
                    }
                    Err(PipelineError::Hls(e)) => {
                        yield Err(e);
                        return;
                    }
                    Err(PipelineError::Aborted) => {
                        yield Err(HlsError::Driver("pipeline aborted".to_string()));
                        return;
                    }
                }
            }

            events.emit_end_of_stream();
        })
    }
}
