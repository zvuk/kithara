use std::{pin::Pin, sync::Arc, time::Instant};

use bytes::Bytes;
use futures::{Stream as FuturesStream, StreamExt};
use kithara_stream::{Message, Source, SourceStream, Stream, StreamError, StreamParams};
use thiserror::Error;
use tokio::sync::{Mutex, broadcast};
use url::Url;

use crate::{
    HlsError, HlsOptions, HlsResult,
    abr::{AbrController, variants_from_master},
    events::{EventEmitter, HlsEvent},
    fetch::FetchManager,
    keys::KeyManager,
    playlist::{PlaylistManager, VariantId},
};

pub type HlsByteStream = Pin<Box<dyn FuturesStream<Item = HlsResult<Bytes>> + Send>>;

#[derive(Debug, Error)]
pub enum DriverError {
    #[error("Source error: {0}")]
    Source(#[from] SourceError),

    #[error("Stream error: {0}")]
    Stream(#[from] kithara_stream::StreamError<SourceError>),
}

#[derive(Debug, Error)]
pub enum SourceError {
    #[error("HLS error: {0}")]
    Hls(#[from] crate::HlsError),
}

pub struct HlsDriver {
    options: HlsOptions,
    master_url: Url,
    playlist_manager: Arc<PlaylistManager>,
    fetch_manager: Arc<FetchManager>,
    key_manager: Arc<KeyManager>,
    abr_controller: Arc<Mutex<AbrController>>,
    event_emitter: Arc<EventEmitter>,
}

fn media_playlist_init_uri(media: &crate::playlist::MediaPlaylist) -> Option<&str> {
    media.init_segment.as_ref().map(|i| i.uri.as_str())
}

impl HlsDriver {
    pub fn new(
        master_url: Url,
        options: HlsOptions,
        playlist_manager: PlaylistManager,
        fetch_manager: FetchManager,
        key_manager: KeyManager,
        abr_controller: AbrController,
        event_emitter: EventEmitter,
    ) -> Self {
        Self {
            options,
            master_url,
            playlist_manager: Arc::new(playlist_manager),
            fetch_manager: Arc::new(fetch_manager),
            key_manager: Arc::new(key_manager),
            abr_controller: Arc::new(Mutex::new(abr_controller)),
            event_emitter: Arc::new(event_emitter),
        }
    }

    pub fn stream(&self) -> Pin<Box<dyn FuturesStream<Item = HlsResult<Bytes>> + Send + '_>> {
        let source = HlsStream {
            master_url: self.master_url.clone(),
            playlist_manager: Arc::clone(&self.playlist_manager),
            fetch_manager: Arc::clone(&self.fetch_manager),
            key_manager: Arc::clone(&self.key_manager),
            abr_controller: Arc::clone(&self.abr_controller),
            event_emitter: Arc::clone(&self.event_emitter),
        };

        let params = StreamParams {
            offline_mode: false,
        };

        let s = Stream::new(source, params).into_byte_stream();
        Box::pin(s.map(|r| {
            r.map_err(|e| match e {
                StreamError::SeekNotSupported => crate::HlsError::Unimplemented,
                StreamError::ChannelClosed => crate::HlsError::Driver("channel closed".to_string()),
                StreamError::WriterJoin(msg) => crate::HlsError::Driver(msg),
                StreamError::Source(SourceError::Hls(hls)) => hls,
            })
        }))
    }

    pub fn events(&self) -> broadcast::Receiver<HlsEvent> {
        self.event_emitter.subscribe()
    }
}

struct HlsStream {
    master_url: Url,
    playlist_manager: Arc<PlaylistManager>,
    fetch_manager: Arc<FetchManager>,
    key_manager: Arc<KeyManager>,
    abr_controller: Arc<Mutex<AbrController>>,
    event_emitter: Arc<EventEmitter>,
}

impl Source for HlsStream {
    type Error = SourceError;
    type Control = ();

    fn open(
        &mut self,
        params: StreamParams,
    ) -> Result<SourceStream<Self::Control, Self::Error>, StreamError<Self::Error>> {
        let master_url = self.master_url.clone();
        let playlist_manager = Arc::clone(&self.playlist_manager);
        let fetch_manager = Arc::clone(&self.fetch_manager);
        let abr_controller = Arc::clone(&self.abr_controller);
        let event_emitter = Arc::clone(&self.event_emitter);

        let _offline_mode = params.offline_mode;

        Ok(Box::pin(async_stream::stream! {

            let master_playlist = playlist_manager
                .fetch_master_playlist(&master_url)
                .await
                .map_err(|e| StreamError::Source(SourceError::Hls(e)))?;

            let variants = variants_from_master(&master_playlist);

            let mut buffer_level_secs: f64 = 0.0;
            let mut pending_applied: Option<(usize, usize, crate::abr::AbrReason)> = None;

            let mut current_variant = {
                let abr = abr_controller.lock().await;
                abr.current_variant()
            };

            let now = Instant::now();
            let initial_decision = {
                let abr = abr_controller.lock().await;
                abr.decide_for_master(&master_playlist, &variants, buffer_level_secs, now)
            };
            if initial_decision.changed && initial_decision.target_variant_index != current_variant {
                event_emitter.emit_variant_decision(
                    current_variant,
                    initial_decision.target_variant_index,
                    initial_decision.reason,
                );
                pending_applied = Some((
                    current_variant,
                    initial_decision.target_variant_index,
                    initial_decision.reason,
                ));

                {
                    let mut abr = abr_controller.lock().await;
                    abr.apply(&initial_decision, now);
                }
                current_variant = initial_decision.target_variant_index;
            }

            let mut variant_uri: String = master_playlist
                .variants
                .get(current_variant)
                .map(|v| v.uri.clone())
                .ok_or_else(|| {
                    StreamError::Source(SourceError::Hls(HlsError::VariantNotFound(format!(
                        "Variant index {}",
                        current_variant
                    ))))
                })?;

            let mut media_url = playlist_manager
                .resolve_url(&master_url, &variant_uri)
                .map_err(|e| StreamError::Source(SourceError::Hls(e)))?;

            let mut media_playlist = playlist_manager
                .fetch_media_playlist(&media_url, VariantId(current_variant))
                .await
                .map_err(|e| StreamError::Source(SourceError::Hls(e)))?;

            let mut current_init_uri = media_playlist_init_uri(&media_playlist).map(str::to_string);
            let mut pending_init_uri: Option<String> = None;
            if let Some(init_uri) = current_init_uri.as_deref() {
                let init_url = playlist_manager
                    .resolve_url(&media_url, init_uri)
                    .map_err(|e| StreamError::Source(SourceError::Hls(e)))?;
                let bytes = fetch_manager
                    .fetch_init_segment(&init_url)
                    .await
                    .map_err(|e| StreamError::Source(SourceError::Hls(e)))?;
                yield Ok(Message::Data(bytes));
            }

            let segment_count = media_playlist.segments.len();
            for segment_index in 0..segment_count {
                let now = Instant::now();
                let decision = {
                    let abr = abr_controller.lock().await;
                    abr.decide_for_master(&master_playlist, &variants, buffer_level_secs, now)
                };

                if decision.changed && decision.target_variant_index != current_variant {
                    event_emitter.emit_variant_decision(
                        current_variant,
                        decision.target_variant_index,
                        decision.reason,
                    );
                    pending_applied = Some((current_variant, decision.target_variant_index, decision.reason));

                    {
                        let mut abr = abr_controller.lock().await;
                        abr.apply(&decision, now);
                    }

                    current_variant = decision.target_variant_index;
                    variant_uri = master_playlist
                        .variants
                        .get(current_variant)
                        .map(|v| v.uri.clone())
                        .ok_or_else(|| {
                            StreamError::Source(SourceError::Hls(HlsError::VariantNotFound(format!(
                                "Variant index {}",
                                current_variant
                            ))))
                        })?;

                    media_url = playlist_manager
                        .resolve_url(&master_url, &variant_uri)
                        .map_err(|e| StreamError::Source(SourceError::Hls(e)))?;

                    media_playlist = playlist_manager
                        .fetch_media_playlist(&media_url, VariantId(current_variant))
                        .await
                        .map_err(|e| StreamError::Source(SourceError::Hls(e)))?;

                    let new_init_uri = media_playlist_init_uri(&media_playlist).map(str::to_string);
                    if new_init_uri != current_init_uri {
                        pending_init_uri = new_init_uri.clone();
                        current_init_uri = new_init_uri;
                    }
                }

                if let Some((from, to, reason)) = pending_applied.take() {
                    event_emitter.emit_variant_applied(from, to, reason);
                    if let Some(init_uri) = pending_init_uri.take() {
                        let init_url = playlist_manager
                            .resolve_url(&media_url, &init_uri)
                            .map_err(|e| StreamError::Source(SourceError::Hls(e)))?;
                        let init = fetch_manager
                            .fetch_init_segment(&init_url)
                            .await
                            .map_err(|e| StreamError::Source(SourceError::Hls(e)))?;
                        yield Ok(Message::Data(init));
                    }
                }

                let fetch = fetch_manager
                    .fetch_segment_with_meta(&media_playlist, segment_index, &media_url, None)
                    .await;
                match fetch {
                    Ok(fetch) => {
                        let at = Instant::now();
                        if fetch.duration != std::time::Duration::ZERO {
                            let bytes_per_second = fetch.bytes.len() as f64 / fetch.duration.as_secs_f64();
                            event_emitter.emit_throughput_sample(bytes_per_second);
                        }

                        {
                            let mut abr = abr_controller.lock().await;
                            abr.push_throughput_sample(crate::abr::ThroughputSample {
                                bytes: fetch.bytes.len() as u64,
                                duration: fetch.duration,
                                at,
                                source: fetch.source,
                            });
                        }

                        if let Some(seg) = media_playlist.segments.get(segment_index) {
                            buffer_level_secs += seg.duration.as_secs_f64();
                        }

                        yield Ok(Message::Data(fetch.bytes));
                    }
                    Err(e) => {
                        yield Err(StreamError::Source(SourceError::Hls(e)));
                        return;
                    }
                }
            }

            event_emitter.emit_end_of_stream();
        }))
    }

    fn seek_bytes(&mut self, _pos: u64) -> Result<(), StreamError<Self::Error>> {
        Err(StreamError::SeekNotSupported)
    }

    fn supports_seek(&self) -> bool {
        false
    }
}
