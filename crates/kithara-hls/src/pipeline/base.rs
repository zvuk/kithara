use std::{
    collections::HashMap,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll},
};

use async_stream::stream;
use futures::{Stream, StreamExt};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use url::Url;

use super::types::{
    PipelineError, PipelineEvent, PipelineResult, SegmentMeta, SegmentPayload, SegmentStream,
};
use crate::{
    HlsError,
    abr::AbrController,
    fetch::FetchManager,
    playlist::{MediaPlaylist, PlaylistManager, VariantId},
};

#[derive(Debug, Clone)]
pub struct PlaylistSet {
    media: HashMap<usize, (Url, MediaPlaylist)>,
}

impl PlaylistSet {
    pub fn new(media: HashMap<usize, (Url, MediaPlaylist)>) -> Self {
        Self { media }
    }

    pub fn media_playlist(&self, variant_index: usize) -> Option<(Url, MediaPlaylist)> {
        self.media.get(&variant_index).cloned()
    }
}

/// Базовый слой: выбирает вариант, итерирует сегменты, реагирует на команды (seek/force/shutdown),
/// публикует события в общий канал.
pub struct BaseStream {
    fetch: Arc<FetchManager>,
    playlists: Arc<PlaylistSet>,
    cancel: CancellationToken,
    events: broadcast::Sender<PipelineEvent>,
    abr_controller: Option<AbrController>,
    // Shared current variant tracker owned by ABR controller.
    current_variant: Arc<AtomicUsize>,
    previous_variant: usize,
    inner: Pin<Box<dyn Stream<Item = PipelineResult<SegmentPayload>> + Send>>,
}

impl BaseStream {
    pub fn new(
        fetch: Arc<FetchManager>,
        playlists: Arc<PlaylistSet>,
        current_variant: Arc<AtomicUsize>,
        cancel: CancellationToken,
    ) -> Self {
        Self::with_abr_controller(fetch, playlists, current_variant, cancel, None)
    }

    pub fn with_abr_controller(
        fetch: Arc<FetchManager>,
        playlists: Arc<PlaylistSet>,
        current_variant: Arc<AtomicUsize>,
        cancel: CancellationToken,
        abr_controller: Option<AbrController>,
    ) -> Self {
        let (events, _) = broadcast::channel(256);
        let initial_variant = current_variant.load(Ordering::Relaxed);
        // BaseStream uses the shared current_variant rather than owning its own copy.

        let inner = Self::variant_stream(
            fetch.clone(),
            playlists.clone(),
            events.clone(),
            initial_variant,
            initial_variant,
            0,
        );

        Self {
            fetch,
            playlists,
            cancel,
            events,
            abr_controller,
            current_variant,
            previous_variant: initial_variant,
            inner,
        }
    }

    pub async fn build(
        master_url: Url,
        fetch: Arc<FetchManager>,
        playlist_manager: Arc<PlaylistManager>,
        current_variant: Arc<AtomicUsize>,
        cancel: CancellationToken,
    ) -> Result<Self, HlsError> {
        let master_playlist = playlist_manager.fetch_master_playlist(&master_url).await?;

        let mut media: HashMap<usize, (Url, MediaPlaylist)> = HashMap::new();
        for (idx, variant) in master_playlist.variants.iter().enumerate() {
            let variant_url = playlist_manager.resolve_url(&master_url, &variant.uri)?;
            let media_playlist = playlist_manager
                .fetch_media_playlist(&variant_url, VariantId(idx))
                .await?;
            media.insert(idx, (variant_url, media_playlist));
        }

        let playlists = Arc::new(PlaylistSet::new(media));

        Ok(Self::new(fetch, playlists, current_variant, cancel))
    }

    fn variant_stream(
        fetch: Arc<FetchManager>,
        playlists: Arc<PlaylistSet>,
        events: broadcast::Sender<PipelineEvent>,
        from_variant: usize,
        to_variant: usize,
        start_from: usize,
    ) -> Pin<Box<dyn Stream<Item = PipelineResult<SegmentPayload>> + Send>> {
        let playlist_pair = playlists.media_playlist(to_variant);
        let Some((media_url, media_playlist)) = playlist_pair else {
            return Box::pin(stream! {
                yield Err(PipelineError::Hls(HlsError::VariantNotFound(format!("variant {}", to_variant))));
            });
        };

        let init_segment = media_playlist.init_segment.clone();
        let segments = media_playlist.segments.clone();
        let mut enumerated = fetch
            .stream_segment_sequence(media_playlist, &media_url, None)
            .enumerate();

        Box::pin(stream! {
            let init_segment = init_segment.clone();
            // Send VariantApplied event when starting a new variant stream
            let _ = events.send(PipelineEvent::VariantApplied {
                from: from_variant,
                to: to_variant,
            });

            if start_from == 0 {
                if let Some(init) = init_segment {
                    let init_url = match media_url.join(&init.uri) {
                        Ok(url) => url,
                        Err(e) => {
                            yield Err(PipelineError::Hls(HlsError::InvalidUrl(format!("Failed to resolve init URL: {}", e))));
                            return;
                        }
                    };

                    match fetch.fetch_init_segment_resource(&init_url).await {
                        Ok(init_fetch) => {
                            let meta = SegmentMeta {
                                variant: to_variant,
                                segment_index: usize::MAX,
                                url: init_url.clone(),
                                duration: None,
                            };
                            let _ = events.send(PipelineEvent::SegmentReady {
                                variant: to_variant,
                                segment_index: usize::MAX,
                            });
                            yield Ok(SegmentPayload { meta, bytes: init_fetch.bytes });
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
                    Ok(bytes) => {
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
                            url: segment_url,
                            duration: Some(segment.duration),
                        };
                        let _ = events.send(PipelineEvent::SegmentReady {
                            variant: to_variant,
                            segment_index: idx,
                        });
                        yield Ok(SegmentPayload { meta, bytes });
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
        let variant = self.current_variant.load(Ordering::Relaxed);
        self.inner = Self::variant_stream(
            self.fetch.clone(),
            self.playlists.clone(),
            self.events.clone(),
            self.previous_variant,
            variant,
            segment_index,
        );
        self.previous_variant = variant;
        let _ = self.events.send(PipelineEvent::StreamReset);
    }

    pub fn force_variant(&mut self, variant_index: usize) {
        let from = self.current_variant.swap(variant_index, Ordering::Relaxed);
        if let Some(abr) = self.abr_controller.as_mut() {
            abr.set_current_variant(variant_index);
        }
        let _ = self.events.send(PipelineEvent::VariantSelected {
            from,
            to: variant_index,
        });
        self.inner = Self::variant_stream(
            self.fetch.clone(),
            self.playlists.clone(),
            self.events.clone(),
            from,
            variant_index,
            0,
        );
        self.previous_variant = variant_index;
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

        this.inner.as_mut().poll_next(cx)
    }
}

impl SegmentStream for BaseStream {
    fn event_sender(&self) -> broadcast::Sender<PipelineEvent> {
        self.events.clone()
    }
}
