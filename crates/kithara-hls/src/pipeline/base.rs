use std::{
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::{Duration, Instant},
};

use async_stream::stream;
use bytes::Bytes;
use futures::{Stream, StreamExt, stream};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use url::Url;

use super::types::{
    PipelineError, PipelineEvent, PipelineResult, PipelineStream, SegmentMeta, SegmentPayload,
};
use crate::{
    HlsError, HlsResult,
    abr::{AbrController, AbrReason, ThroughputSample, ThroughputSampleSource},
    fetch::FetchManager,
    keys::KeyManager,
    playlist::{EncryptionMethod, KeyInfo, PlaylistManager, VariantId},
};

/// Базовый слой: выбирает вариант, итерирует сегменты, расшифровывает при необходимости,
/// реагирует на команды (seek/force/shutdown), публикует события в общий канал.
pub struct BaseStream {
    fetch: Arc<FetchManager>,
    playlist_manager: Arc<PlaylistManager>,
    key_manager: Option<Arc<KeyManager>>,
    master_url: Url,
    cancel: CancellationToken,
    events: broadcast::Sender<PipelineEvent>,
    abr_controller: AbrController,
    inner: Pin<Box<dyn Stream<Item = PipelineResult<SegmentPayload>> + Send>>,
}

impl BaseStream {
    pub fn new(
        master_url: Url,
        fetch: Arc<FetchManager>,
        playlist_manager: Arc<PlaylistManager>,
        key_manager: Option<Arc<KeyManager>>,
        abr_controller: AbrController,
        cancel: CancellationToken,
    ) -> Self {
        let (events, _) = broadcast::channel(256);
        let initial_variant = abr_controller.current_variant();

        let mut base = Self {
            fetch,
            playlist_manager,
            key_manager,
            master_url,
            cancel,
            events,
            abr_controller,
            inner: empty_stream(),
        };

        base.inner = base.variant_stream(initial_variant, initial_variant, 0, AbrReason::Initial);
        base
    }

    fn variant_stream(
        &self,
        from_variant: usize,
        to_variant: usize,
        start_from: usize,
        reason: AbrReason,
    ) -> Pin<Box<dyn Stream<Item = PipelineResult<SegmentPayload>> + Send>> {
        let fetch = self.fetch.clone();
        let playlist_manager = self.playlist_manager.clone();
        let key_manager = self.key_manager.clone();
        let master_url = self.master_url.clone();
        let events = self.events.clone();
        let mut abr_controller = self.abr_controller.clone();
        Box::pin(stream! {
            let mut current_from = from_variant;
            let mut current_to = to_variant;
            let mut current_start = start_from;
            let mut current_reason = reason;

            loop {
                let variants = match playlist_manager.variants(&master_url).await {
                    Ok(v) => v,
                    Err(e) => {
                        yield Err(PipelineError::Hls(e));
                        return;
                    }
                };

                if current_to >= variants.len() {
                    yield Err(PipelineError::Hls(HlsError::VariantNotFound(format!("variant {}", current_to))));
                    return;
                }

                if current_from != current_to {
                    let _ = events.send(PipelineEvent::VariantApplied {
                        from: current_from,
                        to: current_to,
                        reason: current_reason,
                    });
                }

                let master = match playlist_manager.master_playlist(&master_url).await {
                    Ok(m) => m,
                    Err(e) => {
                        yield Err(PipelineError::Hls(e));
                        return;
                    }
                };

                let variant_uri = match master.variants.get(current_to) {
                    Some(v) => v.uri.clone(),
                    None => {
                        yield Err(PipelineError::Hls(HlsError::VariantNotFound(format!("variant {}", current_to))));
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
                    .media_playlist(&media_url, VariantId(current_to))
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

                let need_init = current_from != current_to || current_start == 0;
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
                                    variant: current_to,
                                    segment_index: usize::MAX,
                                    sequence: media_playlist.media_sequence,
                                    url: init_url,
                                    duration: None,
                                    key: init.key.clone(),
                                };
                                let _ = events.send(PipelineEvent::SegmentReady {
                                    variant: current_to,
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

                let mut enumerated = fetch
                    .stream_segment_sequence(
                        &media_playlist,
                        &media_url,
                        None,
                    )
                    .enumerate();

                let mut switched = false;
                let mut next_from = current_from;
                let mut next_to = current_to;
                let mut next_start = 0;
                let mut next_reason = current_reason;

                while let Some((idx, item)) = enumerated.next().await {
                    if idx < current_start {
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
                                variant: current_to,
                                segment_index: idx,
                                sequence: segment.sequence,
                                url: segment_url,
                                duration: Some(fetch_bytes.duration.max(Duration::from_millis(1))),
                                key: segment.key.clone(),
                            };

                            let now = Instant::now();
                            let duration = meta
                                .duration
                                .unwrap_or(Duration::from_millis(1))
                                .max(Duration::from_millis(1));
                            let sample = ThroughputSample {
                                bytes: fetch_bytes.bytes.len() as u64,
                                duration,
                                at: now,
                                source: ThroughputSampleSource::Network,
                            };
                            abr_controller.push_throughput_sample(sample);
                            let decision =
                                abr_controller.decide(&variants, duration.as_secs_f64(), now);

                            if decision.changed {
                                let from = abr_controller.current_variant();
                                abr_controller.apply(&decision, now);
                                let _ = events.send(PipelineEvent::VariantSelected {
                                    from,
                                    to: decision.target_variant_index,
                                    reason: decision.reason,
                                });
                                let _ = events.send(PipelineEvent::VariantApplied {
                                    from,
                                    to: decision.target_variant_index,
                                    reason: decision.reason,
                                });

                                next_from = from;
                                next_to = decision.target_variant_index;
                                next_start = match meta.segment_index {
                                    usize::MAX => 0,
                                    idx => idx.saturating_add(1),
                                };
                                next_reason = decision.reason;
                                switched = true;
                            }

                            // Decrypt if needed
                            let final_bytes = if let Some(ref km) = key_manager {
                                match decrypt_segment(km, &meta, fetch_bytes.bytes).await {
                                    Ok(decrypted) => {
                                        let _ = events.send(PipelineEvent::Decrypted {
                                            variant: meta.variant,
                                            segment_index: meta.segment_index,
                                        });
                                        decrypted
                                    }
                                    Err(e) => {
                                        yield Err(PipelineError::Hls(e));
                                        break;
                                    }
                                }
                            } else {
                                fetch_bytes.bytes
                            };

                            let _ = events.send(PipelineEvent::SegmentReady {
                                variant: current_to,
                                segment_index: idx,
                            });
                            yield Ok(SegmentPayload {
                                meta,
                                bytes: final_bytes,
                            });

                            if switched {
                                break;
                            }
                        }
                        Err(err) => {
                            yield Err(PipelineError::Hls(err));
                            break;
                        }
                    }
                }

                if switched {
                    current_from = next_from;
                    current_to = next_to;
                    current_start = next_start;
                    current_reason = next_reason;
                    continue;
                }

                break;
            }
        })
    }

    pub fn seek(&mut self, segment_index: usize) {
        let variant = self.abr_controller.current_variant();
        self.inner =
            self.variant_stream(variant, variant, segment_index, AbrReason::ManualOverride);
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
        self.inner = self.variant_stream(from, variant_index, 0, crate::AbrReason::ManualOverride);
        let _ = self.events.send(PipelineEvent::StreamReset);
    }
}

fn resolve_key_url(key_info: &KeyInfo, segment_url: &Url) -> HlsResult<Url> {
    let key_uri = key_info
        .uri
        .as_ref()
        .ok_or_else(|| HlsError::InvalidUrl("missing key URI".to_string()))?;

    if key_uri.starts_with("http://") || key_uri.starts_with("https://") {
        Url::parse(key_uri).map_err(|e| HlsError::InvalidUrl(format!("Invalid key URL: {e}")))
    } else {
        segment_url
            .join(key_uri)
            .map_err(|e| HlsError::InvalidUrl(format!("Failed to resolve key URL: {e}")))
    }
}

fn derive_iv(key_info: &KeyInfo, sequence: u64) -> [u8; 16] {
    if let Some(iv) = key_info.iv {
        return iv;
    }
    let mut iv = [0u8; 16];
    iv[8..].copy_from_slice(&sequence.to_be_bytes());
    iv
}

async fn decrypt_segment(
    key_manager: &KeyManager,
    meta: &SegmentMeta,
    bytes: Bytes,
) -> HlsResult<Bytes> {
    let Some(ref seg_key) = meta.key else {
        return Ok(bytes);
    };

    if !matches!(seg_key.method, EncryptionMethod::Aes128) {
        return Ok(bytes);
    }

    let Some(ref key_info) = seg_key.key_info else {
        return Ok(bytes);
    };

    let key_url = resolve_key_url(key_info, &meta.url)?;
    let iv = derive_iv(key_info, meta.sequence);

    key_manager.decrypt(&key_url, Some(iv), bytes).await
}

impl Stream for BaseStream {
    type Item = PipelineResult<SegmentPayload>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();

        if this.cancel.is_cancelled() {
            return Poll::Ready(None);
        }

        this.inner.as_mut().poll_next(cx)
    }
}

fn empty_stream() -> Pin<Box<dyn Stream<Item = PipelineResult<SegmentPayload>> + Send>> {
    Box::pin(stream::empty())
}

impl PipelineStream for BaseStream {
    fn event_sender(&self) -> broadcast::Sender<PipelineEvent> {
        self.events.clone()
    }
}
