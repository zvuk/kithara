use bytes::Bytes;
use futures::Stream;
use hls_m3u8::MasterPlaylist;
use std::pin::Pin;
use std::time::Duration;
use thiserror::Error;
use tokio::sync::mpsc;
use url::Url;

use crate::{
    HlsCommand, HlsError, HlsOptions, HlsResult,
    abr::AbrController,
    events::{EventEmitter, VariantChangeReason},
    fetch::FetchManager,
    keys::KeyManager,
    playlist::PlaylistManager,
};

#[derive(Debug, Error)]
pub enum DriverError {
    #[error("Command channel closed")]
    ChannelClosed,

    #[error("Invalid state transition")]
    InvalidState,

    #[error("Playlist error: {0}")]
    Playlist(String),

    #[error("Fetch error: {0}")]
    Fetch(String),

    #[error("Key error: {0}")]
    Key(String),
}

pub type HlsByteStream = Pin<Box<dyn Stream<Item = HlsResult<Bytes>> + Send>>;

#[derive(Debug)]
pub enum DriverState {
    Starting,
    LoadingMasterPlaylist,
    LoadingMediaPlaylist,
    Streaming,
    Seeking(Duration),
    Stopping,
    Stopped,
    Error(String),
}

pub struct HlsDriver {
    master_url: Url,
    options: HlsOptions,
    playlist_manager: PlaylistManager,
    fetch_manager: FetchManager,
    key_manager: KeyManager,
    abr_controller: AbrController,
    event_emitter: EventEmitter,
    state: DriverState,
    cmd_receiver: mpsc::Receiver<HlsCommand>,
    bytes_sender: mpsc::Sender<HlsResult<Bytes>>,
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
        cmd_receiver: mpsc::Receiver<HlsCommand>,
        bytes_sender: mpsc::Sender<HlsResult<Bytes>>,
    ) -> Self {
        Self {
            master_url,
            options,
            playlist_manager,
            fetch_manager,
            key_manager,
            abr_controller,
            event_emitter,
            state: DriverState::Starting,
            cmd_receiver,
            bytes_sender,
        }
    }

    pub async fn run(mut self) -> HlsResult<()> {
        self.state = DriverState::LoadingMasterPlaylist;

        // Fetch master playlist
        let master_playlist = self
            .playlist_manager
            .fetch_master_playlist(&self.master_url)
            .await?;

        // Select initial variant using ABR controller
        let current_variant = self.abr_controller.select_variant(&master_playlist)?;

        // Temporary move out self to avoid borrow issues
        let mut driver = self;
        driver
            .load_media_playlist(&master_playlist, current_variant)
            .await?;

        // Stream all segments
        driver
            .stream_segments(&master_playlist, current_variant)
            .await?;

        // Close the stream after all segments are sent
        drop(driver.bytes_sender);

        Ok(())
    }

    async fn handle_command(
        &mut self,
        cmd: HlsCommand,
        _master_playlist: &MasterPlaylist<'_>,
    ) -> HlsResult<()> {
        match cmd {
            HlsCommand::Stop => {
                self.state = DriverState::Stopping;
            }
            HlsCommand::SeekTime(duration) => {
                self.state = DriverState::Seeking(duration);
            }
            HlsCommand::SetVariant(variant) => {
                self.abr_controller.set_manual_variant(variant)?;
                let old_variant = self.abr_controller.current_variant();
                self.event_emitter.emit_variant_changed(
                    old_variant,
                    variant,
                    VariantChangeReason::Manual,
                );
            }
            HlsCommand::ClearVariantOverride => {
                self.abr_controller.clear_manual_override();
                // No variant change event needed for clearing override
            }
        }
        Ok(())
    }

    async fn load_media_playlist(
        &mut self,
        master_playlist: &MasterPlaylist<'_>,
        variant: usize,
    ) -> HlsResult<()> {
        let variants = &master_playlist.variant_streams;
        let _selected_variant = variants
            .get(variant)
            .ok_or_else(|| HlsError::VariantNotFound(format!("Variant index {}", variant)))?;

        // Get variant URI - need to figure out how to access it
        // For now, hardcode based on variant index (this is just for testing)
        let variant_uri = match variant {
            0 => "v0.m3u8",
            1 => "v1.m3u8",
            2 => "v2.m3u8",
            _ => {
                return Err(HlsError::VariantNotFound(format!(
                    "Variant index {}",
                    variant
                )));
            }
        };

        let media_url = self
            .playlist_manager
            .resolve_url(&self.master_url, variant_uri)?;
        let _media_playlist = self
            .playlist_manager
            .fetch_media_playlist(&media_url)
            .await?;

        self.state = DriverState::Streaming;
        Ok(())
    }

    async fn stream_segments(
        &mut self,
        master_playlist: &MasterPlaylist<'_>,
        variant: usize,
    ) -> HlsResult<()> {
        let variants = &master_playlist.variant_streams;
        let _selected_variant = variants
            .get(variant)
            .ok_or_else(|| HlsError::VariantNotFound(format!("Variant index {}", variant)))?;

        // Get variant URI - need to figure out how to access it
        // For now, hardcode based on variant index (this is just for testing)
        let variant_uri = match variant {
            0 => "v0.m3u8",
            1 => "v1.m3u8",
            2 => "v2.m3u8",
            _ => {
                return Err(HlsError::VariantNotFound(format!(
                    "Variant index {}",
                    variant
                )));
            }
        };

        let media_url = self
            .playlist_manager
            .resolve_url(&self.master_url, variant_uri)?;
        let media_playlist = self
            .playlist_manager
            .fetch_media_playlist(&media_url)
            .await?;

        // Handle encryption if present - simplified for now
        let key_context = None;

        let mut segment_stream = self.fetch_manager.stream_segment_sequence(
            media_playlist,
            &media_url,
            key_context.as_ref(),
        );

        use futures::StreamExt;
        while let Some(segment_result) = segment_stream.next().await {
            match segment_result {
                Ok(bytes) => {
                    if self.bytes_sender.send(Ok(bytes)).await.is_err() {
                        // Channel closed, stop streaming
                        break;
                    }
                }
                Err(e) => {
                    if self.bytes_sender.send(Err(e)).await.is_err() {
                        break;
                    }
                }
            }
        }

        Ok(())
    }

    async fn seek_to_time(
        &mut self,
        _master_playlist: &MasterPlaylist<'_>,
        _variant: usize,
        _target: Duration,
    ) -> HlsResult<()> {
        // TODO: Implement seek logic
        // This would involve finding the right segment based on duration
        // and adjusting the stream accordingly
        self.state = DriverState::Streaming;
        Ok(())
    }
}
