use std::{
    collections::HashMap,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use async_stream::stream;
use bytes::Bytes;
use futures::{Stream, StreamExt};
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::{
    HlsError, HlsOptions, HlsResult,
    abr::{self, Variant},
    events::{EventEmitter, HlsEvent},
    fetch::FetchManager,
    pipeline::{
        AbrStream, BaseStream, DrmStream, PipelineError, PipelineEvent, PrefetchStream,
        SegmentPayload, SegmentStream, drm::Decrypter,
    },
    playlist::{MasterPlaylist, MediaPlaylist, PlaylistManager, VariantId},
};

#[derive(Debug, thiserror::Error)]
pub enum DriverError {
    #[error("pipeline error: {0}")]
    Pipeline(#[from] PipelineError),
}

/// Адаптер для FetchManager под новый пайплайн.
struct FetcherAdapter {
    fetch: Arc<FetchManager>,
}

impl crate::pipeline::base::Fetcher for FetcherAdapter {
    fn stream_segment_sequence(
        &self,
        media_playlist: MediaPlaylist,
        base_url: &Url,
    ) -> crate::fetch::SegmentStream<'static> {
        self.fetch
            .stream_segment_sequence(media_playlist, base_url, None)
    }
}

/// Адаптер плейлистов: мастер + заранее загруженные медиа.
struct PlaylistAdapter {
    master: MasterPlaylist,
    media: HashMap<usize, (Url, MediaPlaylist)>,
}

impl PlaylistAdapter {
    fn new(master: MasterPlaylist, media: HashMap<usize, (Url, MediaPlaylist)>) -> Self {
        Self { master, media }
    }
}

impl crate::pipeline::base::PlaylistProvider for PlaylistAdapter {
    fn master_playlist(&self) -> &MasterPlaylist {
        &self.master
    }

    fn media_playlist(&self, variant_index: usize) -> Option<(Url, MediaPlaylist)> {
        self.media.get(&variant_index).cloned()
    }
}

/// No-op DRM placeholder: returns payload unchanged until real DRM arrives.
struct NoopDecrypter;

impl Decrypter for NoopDecrypter {
    fn decrypt(
        &self,
        payload: SegmentPayload,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<SegmentPayload, HlsError>> + Send>>
    {
        Box::pin(async move { Ok(payload) })
    }
}

pub struct HlsDriver {
    options: HlsOptions,
    master_url: Url,
    playlist_manager: Arc<PlaylistManager>,
    fetch_manager: Arc<FetchManager>,
    // key_manager: Arc<KeyManager>, // Temporarily unused, kept for future DRM support
    event_emitter: Arc<EventEmitter>,
    cancel: CancellationToken,
}

impl HlsDriver {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        master_url: Url,
        options: HlsOptions,
        playlist_manager: PlaylistManager,
        fetch_manager: FetchManager,
        event_emitter: EventEmitter,
    ) -> Self {
        Self {
            options,
            master_url,
            playlist_manager: Arc::new(playlist_manager),
            fetch_manager: Arc::new(fetch_manager),
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
        let events = Arc::clone(&self.event_emitter);
        let cancel = self.cancel.clone();
        let opts = self.options.clone();
        let master_url = self.master_url.clone();

        Box::pin(stream! {
            // 1) Загружаем мастер плейлист.
            let master_playlist = match playlist_manager.fetch_master_playlist(&master_url).await {
                Ok(m) => m,
                Err(e) => {
                    yield Err(e);
                    return;
                }
            };

            // 2) Загружаем все медиа плейлисты по вариантам (для быстрой смены).
            let mut media_map: HashMap<usize, (Url, MediaPlaylist)> = HashMap::new();
            for (idx, variant) in master_playlist.variants.iter().enumerate() {
                let variant_url = match playlist_manager.resolve_url(&master_url, &variant.uri) {
                    Ok(url) => url,
                    Err(e) => {
                        yield Err(e);
                        return;
                    }
                };

                match playlist_manager
                    .fetch_media_playlist(&variant_url, VariantId(idx))
                    .await
                {
                    Ok(media) => {
                        media_map.insert(idx, (variant_url.clone(), media));
                    }
                    Err(e) => {
                        yield Err(e);
                        return;
                    }
                }
            }

            // 3) Адаптеры для пайплайна.
            let playlist_adapter = Arc::new(PlaylistAdapter::new(master_playlist.clone(), media_map));
            let fetch_adapter = Arc::new(FetcherAdapter { fetch: fetch_manager.clone() });

            // 4) Создаем конфигурацию ABR
            let abr_config = abr::AbrConfig {
                min_buffer_for_up_switch_secs: f64::from(opts.abr_min_buffer_for_up_switch),
                down_switch_buffer_secs: f64::from(opts.abr_down_switch_buffer),
                throughput_safety_factor: f64::from(opts.abr_throughput_safety_factor),
                up_hysteresis_ratio: f64::from(opts.abr_up_hysteresis_ratio),
                down_hysteresis_ratio: f64::from(opts.abr_down_hysteresis_ratio),
                min_switch_interval: opts.abr_min_switch_interval,
                initial_variant_index: opts.abr_initial_variant_index,
                sample_window: std::time::Duration::from_secs(30),
            };

            // 5) Определяем начальный вариант
            let variants: Vec<Variant> = abr::variants_from_master(&master_playlist);
            let now = std::time::Instant::now();

            // Shared current_variant for ABR + base layer
            let current_variant = Arc::new(AtomicUsize::new(
                opts.abr_initial_variant_index.unwrap_or(0),
            ));

            // Создаем ABR контроллер для начального решения
            let abr_controller = abr::AbrController::new(
                abr_config.clone(),
                opts.variant_stream_selector.clone(),
                current_variant.clone(),
            );

            let initial_variant = abr_controller
                .decide_for_master(&master_playlist, &variants, 0.0, now)
                .target_variant_index;
            current_variant.store(initial_variant, Ordering::Relaxed);

            // 6) Prefetch теперь управляется PrefetchStream слоем, который заполняет
            // буферы последовательно (один за другим) согласно prefetch_buffer_size.

            let base = BaseStream::new(
                fetch_adapter,
                playlist_adapter,
                current_variant.clone(),
                cancel.clone(),
            );
            let abr_layer = AbrStream::new(base, abr_controller);
            let drm_layer = DrmStream::new(
                abr_layer,
                Some(Arc::new(NoopDecrypter)),
                cancel.clone(),
            );
            let prefetch_capacity = opts.prefetch_buffer_size.unwrap_or(1).max(1);
            let pipeline = PrefetchStream::new(drm_layer, prefetch_capacity, cancel.clone());

            // 6) Подписка на события пайплайна -> HlsEvent.
            let mut ev_rx = pipeline.event_sender().subscribe();
            let events_clone = Arc::clone(&events);
            tokio::spawn(async move {
                while let Ok(ev) = ev_rx.recv().await {
                    match ev {
                        PipelineEvent::VariantSelected { from, to } => {
                            events_clone.emit_variant_decision(from, to, abr::AbrReason::ManualOverride);
                        }
                        PipelineEvent::VariantApplied { from, to } => {
                            events_clone.emit_variant_applied(from, to, abr::AbrReason::ManualOverride);
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
