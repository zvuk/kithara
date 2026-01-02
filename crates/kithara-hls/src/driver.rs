use bytes::Bytes;
use futures::{Stream as FuturesStream, StreamExt};
use kithara_stream::{Message, Source, SourceStream, Stream, StreamError, StreamParams};
use std::pin::Pin;
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::Mutex;
use url::Url;

use crate::{
    HlsOptions, HlsResult, abr::AbrController, events::EventEmitter, fetch::FetchManager,
    keys::KeyManager, playlist::PlaylistManager,
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
            offline_mode: self.options.offline_mode,
        };

        let s = Stream::new(source, params).into_byte_stream();
        Box::pin(s.map(|r| {
            r.map_err(|e| match e {
                StreamError::SeekNotSupported => crate::HlsError::Unimplemented,
                StreamError::ChannelClosed => crate::HlsError::Driver("channel closed".to_string()),
                StreamError::Source(SourceError::Hls(hls)) => hls,
            })
        }))
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

        let offline_mode = params.offline_mode;

        Ok(Box::pin(async_stream::stream! {
            if offline_mode {
                yield Err(StreamError::Source(SourceError::Hls(crate::HlsError::OfflineMiss)));
                return;
            }

            let master_playlist = playlist_manager
                .fetch_master_playlist(&master_url)
                .await
                .map_err(|e| StreamError::Source(SourceError::Hls(e)))?;

            let variant = {
                let mut abr = abr_controller.lock().await;
                abr.select_variant(&master_playlist)
                    .map_err(|e| StreamError::Source(SourceError::Hls(e)))?
            };

            // NOTE: variant URI resolution is still the responsibility of the HLS crate;
            // this is intentionally minimal until we TDD the full implementation.
            let variant_uri = match variant {
                0 => "v0.m3u8",
                1 => "v1.m3u8",
                2 => "v2.m3u8",
                _ => {
                    yield Err(StreamError::Source(SourceError::Hls(crate::HlsError::VariantNotFound(format!(
                        "Variant index {}",
                        variant
                    )))));
                    return;
                }
            };

            let media_url = playlist_manager
                .resolve_url(&master_url, variant_uri)
                .map_err(|e| StreamError::Source(SourceError::Hls(e)))?;

            let media_playlist = playlist_manager
                .fetch_media_playlist(&media_url)
                .await
                .map_err(|e| StreamError::Source(SourceError::Hls(e)))?;

            let key_context = None;
            let mut segs = fetch_manager.stream_segment_sequence(
                media_playlist,
                &media_url,
                key_context.as_ref(),
            );

            while let Some(item) = segs.next().await {
                match item {
                    Ok(bytes) => yield Ok(Message::Data(bytes)),
                    Err(e) => {
                        yield Err(StreamError::Source(SourceError::Hls(e)));
                        return;
                    }
                }
            }
        }))
    }

    fn seek_bytes(&mut self, _pos: u64) -> Result<(), StreamError<Self::Error>> {
        Err(StreamError::SeekNotSupported)
    }

    fn supports_seek(&self) -> bool {
        false
    }
}
