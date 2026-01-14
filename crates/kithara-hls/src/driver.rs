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
    pipeline::{BaseStream, PipelineError, PipelineEvent, PipelineStream, PrefetchStream},
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
        playlist_manager: PlaylistManager,
        fetch_manager: Arc<FetchManager>,
        key_manager: Arc<KeyManager>,
        event_emitter: EventEmitter,
    ) -> Self {
        Self {
            options,
            master_url,
            playlist_manager: Arc::new(playlist_manager),
            fetch_manager,
            key_manager: Arc::clone(&key_manager),
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

            // 3) Базовый слой сам загружает мастер и медиаплейлисты, расшифровывает при необходимости.
            let base_master_url = master_url.clone();
            let base = BaseStream::new(
                base_master_url,
                fetch_manager.clone(),
                playlist_manager.clone(),
                Some(Arc::clone(&key_manager)),
                abr_controller.clone(),
                cancel.clone(),
            );
            let prefetch_capacity = opts.prefetch_buffer_size.unwrap_or(1).max(1);
            let pipeline = PrefetchStream::new(base, prefetch_capacity, cancel.clone());

            // 6) Подписка на события пайплайна -> HlsEvent.
            let ev_rx = pipeline.event_sender().subscribe();
            let events_clone = Arc::clone(&events);
            tokio::spawn(async move {
                let mut ev_rx = ev_rx;
                while let Ok(ev) = ev_rx.recv().await {
                    match ev {
                        PipelineEvent::VariantSelected { from, to, reason } => {
                            events_clone.emit_variant_decision(from, to, reason);
                        }
                        PipelineEvent::VariantApplied { from, to, reason } => {
                            events_clone.emit_variant_applied(from, to, reason);
                        }
                        PipelineEvent::SegmentReady { variant, segment_index } => {
                            events_clone.emit_segment_start(variant, segment_index, 0);
                        }
                        PipelineEvent::Decrypted { .. } => {
                            // Нет отдельного HlsEvent, пропускаем.
                        }
                        PipelineEvent::Prefetched { .. } => {
                            // Нет отдельного HlsEvent, пропускаем.
                        }
                        PipelineEvent::StreamReset => {
                            // Ничего не делаем: событие служебное для внутреннего пайплайна.
                        }
                    }
                }
            });

            // 7) Воркер пайплайна пишет байты в очередь; внешний поток читает из неё.
            let mut data_stream = Box::pin(pipeline);
            let (tx, mut rx) = mpsc::channel(8);
            tokio::spawn(async move {
                while let Some(item) = data_stream.next().await {
                    if tx.send(item).await.is_err() {
                        break;
                    }
                }
            });

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
