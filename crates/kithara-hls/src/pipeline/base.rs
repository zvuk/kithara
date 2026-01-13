use std::{
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::{Duration, Instant},
};

use async_stream::stream;
use futures::{Stream, StreamExt};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use url::Url;

use super::types::{
    PipelineError, PipelineEvent, PipelineResult, PipelineStream, SegmentMeta, SegmentPayload,
};
use crate::{
    HlsError,
    abr::{
        AbrController, AbrReason, ThroughputSample, ThroughputSampleSource, Variant,
        variants_from_master,
    },
    fetch::FetchManager,
    playlist::{PlaylistManager, VariantId},
};

/// Базовый слой: выбирает вариант, итерирует сегменты, реагирует на команды (seek/force/shutdown),
/// публикует события в общий канал.
pub struct BaseStream {
    fetch: Arc<FetchManager>,
    playlist_manager: Arc<PlaylistManager>,
    master_url: Url,
    cancel: CancellationToken,
    events: broadcast::Sender<PipelineEvent>,
    abr_controller: AbrController,
    variants: Vec<Variant>,
    inner: Pin<Box<dyn Stream<Item = PipelineResult<SegmentPayload>> + Send>>,
}

impl BaseStream {
    pub fn new(
        master_url: Url,
        fetch: Arc<FetchManager>,
        playlist_manager: Arc<PlaylistManager>,
        abr_controller: AbrController,
        variants: Vec<Variant>,
        cancel: CancellationToken,
    ) -> Self {
        Self::with_abr_controller(
            master_url,
            fetch,
            playlist_manager,
            abr_controller,
            variants,
            cancel,
        )
    }

    pub fn with_abr_controller(
        master_url: Url,
        fetch: Arc<FetchManager>,
        playlist_manager: Arc<PlaylistManager>,
        abr_controller: AbrController,
        variants: Vec<Variant>,
        cancel: CancellationToken,
    ) -> Self {
        let (events, _) = broadcast::channel(256);
        let initial_variant = abr_controller.current_variant();

        let inner = Self::variant_stream(
            fetch.clone(),
            playlist_manager.clone(),
            master_url.clone(),
            events.clone(),
            initial_variant,
            initial_variant,
            0,
            AbrReason::Initial,
        );

        Self {
            fetch,
            playlist_manager,
            master_url,
            cancel,
            events,
            abr_controller,
            variants,
            inner,
        }
    }

    pub async fn build(
        master_url: Url,
        fetch: Arc<FetchManager>,
        playlist_manager: Arc<PlaylistManager>,
        abr_controller: AbrController,
        cancel: CancellationToken,
    ) -> Result<Self, HlsError> {
        let master_playlist = playlist_manager.master_playlist(&master_url).await?;
        let variants = variants_from_master(&master_playlist);

        Ok(Self::new(
            master_url,
            fetch,
            playlist_manager,
            abr_controller,
            variants,
            cancel,
        ))
    }

    fn variant_stream(
        fetch: Arc<FetchManager>,
        playlist_manager: Arc<PlaylistManager>,
        master_url: Url,
        events: broadcast::Sender<PipelineEvent>,
        from_variant: usize,
        to_variant: usize,
        start_from: usize,
        reason: AbrReason,
    ) -> Pin<Box<dyn Stream<Item = PipelineResult<SegmentPayload>> + Send>> {
        Box::pin(stream! {
            let master = match playlist_manager.master_playlist(&master_url).await {
                Ok(m) => m,
                Err(e) => {
                    yield Err(PipelineError::Hls(e));
                    return;
                }
            };

            let variant_uri = match master.variants.get(to_variant) {
                Some(v) => v.uri.clone(),
                None => {
                    yield Err(PipelineError::Hls(HlsError::VariantNotFound(format!("variant {}", to_variant))));
                    return;
                }
            };

            let media_url = match playlist_manager.resolve_url(&master_url, &variant_uri) {
                Ok(url) => url,
                Err(e) => {
                    yield Err(PipelineError::Hls(e));
                    return;
                }
            };

            let media_playlist = match playlist_manager
                .media_playlist(&media_url, VariantId(to_variant))
                .await
            {
                Ok(p) => p,
                Err(e) => {
                    yield Err(PipelineError::Hls(e));
                    return;
                }
            };

            let init_segment = media_playlist.init_segment.clone();
            let segments = media_playlist.segments.clone();
            let mut enumerated = FetchManager::stream_segment_sequence(
                &*fetch,
                &media_playlist,
                &media_url,
                None,
            )
            .enumerate();

            let init_segment = init_segment.clone();
            // Send VariantApplied event when starting a new variant stream
            if from_variant != to_variant && !matches!(reason, crate::AbrReason::ManualOverride) {
                let _ = events.send(PipelineEvent::VariantApplied {
                    from: from_variant,
                    to: to_variant,
                    reason,
                });
            }

            let need_init = from_variant != to_variant || start_from == 0;
            if need_init {
                if let Some(init) = init_segment {
                    let init_result = async {
                        let init_url = media_url
                            .join(&init.uri)
                            .map_err(|e| HlsError::InvalidUrl(format!("Failed to resolve init URL: {}", e)))?;
                        let init_fetch = fetch.fetch_init_segment_resource(&init_url).await?;
                        Ok::<_, HlsError>((init_url, init_fetch.bytes))
                    }
                    .await;

                    match init_result {
                        Ok((init_url, bytes)) => {
                            let meta = SegmentMeta {
                                variant: to_variant,
                                segment_index: usize::MAX,
                                sequence: media_playlist.media_sequence,
                                url: init_url,
                                duration: None,
                                key: init.key.clone(),
                            };
                            let _ = events.send(PipelineEvent::SegmentReady {
                                variant: to_variant,
                                segment_index: usize::MAX,
                            });
                            yield Ok(SegmentPayload { meta, bytes });
                        }
                        Err(err) => {
                            yield Err(PipelineError::Hls(err));
                            return;
                        }
                    }
                }
            }

            while let Some((idx, item)) = enumerated.next().await {
                if idx < start_from {
                    continue;
                }

                match item {
                    Ok(fetch_bytes) => {
                        let Some(segment) = segments.get(idx) else {
                            yield Err(PipelineError::Hls(HlsError::SegmentNotFound(format!("Index {}", idx))));
                            break;
                        };

                        let segment_url = match media_url.join(&segment.uri) {
                            Ok(url) => url,
                            Err(e) => {
                                yield Err(PipelineError::Hls(HlsError::InvalidUrl(format!("Failed to resolve segment URL: {}", e))));
                                break;
                            }
                        };

                        let meta = SegmentMeta {
                            variant: to_variant,
                            segment_index: idx,
                            sequence: segment.sequence,
                            url: segment_url,
                            duration: Some(fetch_bytes.duration.max(Duration::from_millis(1))),
                            key: segment.key.clone(),
                        };
                        let _ = events.send(PipelineEvent::SegmentReady {
                            variant: to_variant,
                            segment_index: idx,
                        });
                        yield Ok(SegmentPayload {
                            meta,
                            bytes: fetch_bytes.bytes,
                        });
                    }
                    Err(err) => {
                        yield Err(PipelineError::Hls(err));
                        break;
                    }
                }
            }
        })
    }

    pub fn seek(&mut self, segment_index: usize) {
        let variant = self.abr_controller.current_variant();
        self.inner = Self::variant_stream(
            self.fetch.clone(),
            self.playlist_manager.clone(),
            self.master_url.clone(),
            self.events.clone(),
            variant,
            variant,
            segment_index,
            AbrReason::ManualOverride,
        );
        let _ = self.events.send(PipelineEvent::StreamReset);
    }

    pub fn force_variant(&mut self, variant_index: usize) {
        let from = self.abr_controller.current_variant();
        self.abr_controller.set_current_variant(variant_index);
        let _ = self.events.send(PipelineEvent::VariantSelected {
            from,
            to: variant_index,
            reason: crate::AbrReason::ManualOverride,
        });
        let _ = self.events.send(PipelineEvent::VariantApplied {
            from,
            to: variant_index,
            reason: crate::AbrReason::ManualOverride,
        });
        self.inner = Self::variant_stream(
            self.fetch.clone(),
            self.playlist_manager.clone(),
            self.master_url.clone(),
            self.events.clone(),
            from,
            variant_index,
            0,
            AbrReason::ManualOverride,
        );
        let _ = self.events.send(PipelineEvent::StreamReset);
    }
}

impl Stream for BaseStream {
    type Item = PipelineResult<SegmentPayload>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();

        if this.cancel.is_cancelled() {
            return Poll::Ready(None);
        }

        match this.inner.as_mut().poll_next(cx) {
            Poll::Ready(Some(Ok(payload))) => {
                if let Some(duration) = payload.meta.duration {
                    let duration = duration.max(Duration::from_millis(1));
                    let now = Instant::now();
                    let sample = ThroughputSample {
                        bytes: payload.bytes.len() as u64,
                        duration,
                        at: now,
                        source: ThroughputSampleSource::Network,
                    };
                    this.abr_controller.push_throughput_sample(sample);
                    let decision =
                        this.abr_controller
                            .decide(&this.variants, duration.as_secs_f64(), now);
                    if decision.changed {
                        let from = this.abr_controller.current_variant();
                        this.abr_controller.apply(&decision, now);
                        let _ = this.events.send(PipelineEvent::VariantSelected {
                            from,
                            to: decision.target_variant_index,
                            reason: decision.reason,
                        });
                        let start_from = match payload.meta.segment_index {
                            usize::MAX => 0,
                            idx => idx.saturating_add(1),
                        };
                        this.inner = Self::variant_stream(
                            this.fetch.clone(),
                            this.playlist_manager.clone(),
                            this.master_url.clone(),
                            this.events.clone(),
                            from,
                            decision.target_variant_index,
                            start_from,
                            decision.reason,
                        );
                        let _ = this.events.send(PipelineEvent::StreamReset);
                    }
                }

                Poll::Ready(Some(Ok(payload)))
            }
            other => other,
        }
    }
}

impl PipelineStream for BaseStream {
    fn event_sender(&self) -> broadcast::Sender<PipelineEvent> {
        self.events.clone()
    }
}
