use bytes::Bytes;
use futures::Stream;
use hls_m3u8::{MasterPlaylist, MediaPlaylist};
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};
use thiserror::Error;
use tokio::sync::mpsc;
use url::Url;

use crate::{
    HlsCommand, HlsError, HlsOptions, HlsResult, KeyContext,
    abr::{AbrConfig, AbrController, ThroughputSample},
    events::{EventEmitter, HlsEvent, VariantChangeReason},
    fetch::{FetchManager, SegmentStream},
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

#[derive(Clone, Debug)]
pub enum DriverState {
    Starting,
    LoadingMasterPlaylist,
    LoadingMediaPlaylist,
    Streaming,
    Seeking(Duration),
    Stopping,
    Stopped,
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

        let master_playlist = {
            let playlist = self
                .playlist_manager
                .fetch_master_playlist(&self.master_url)
                .await?;
            playlist
        };

        loop {
            // Check for commands
            if let Ok(Some(cmd)) = self.cmd_receiver.try_recv() {
                if let Err(e) = self.handle_command(cmd, &master_playlist).await {
                    // Emit error event but continue running
                    // TODO: Implement proper error handling
                    continue;
                }
            }

            let current_variant = self.abr_controller.current_variant();
            match self.state {
                DriverState::Starting => {
                    // Select initial variant
                    if let Err(e) = self
                        .load_media_playlist(&master_playlist, current_variant)
                        .await
                    {
                        self.state = DriverState::Error(e.clone());
                        continue;
                    }

                    self.state = DriverState::Buffering;
                }
                DriverState::Buffering => {
                    // TODO: Implement buffering logic
                    self.state = DriverState::Streaming;
                }
                DriverState::Streaming => {
                    if let Err(e) = self
                        .stream_segments(&master_playlist, current_variant)
                        .await
                    {
                        self.state = DriverState::Error(e.clone());
                        continue;
                    }
                }
                DriverState::Seeking(target_time) => {
                    if let Err(e) = self
                        .seek_to_time(&master_playlist, current_variant, target_time)
                        .await
                    {
                        self.state = DriverState::Error(e.clone());
                        continue;
                    }

                    self.state = DriverState::Buffering;
                }
                DriverState::Error(_) => {
                    // TODO: Implement error recovery
                    self.state = DriverState::Starting;
                }
                DriverState::Stopped => break,
                _ => {
                    // Invalid state transition
                    return Err(HlsError::Driver("Invalid state transition".to_string()));
                }
            }

            match self.state {
                DriverState::LoadingMediaPlaylist => {
                    self.load_media_playlist(&master_playlist, current_variant)
                        .await?;
                }
                DriverState::Streaming => {
                    self.stream_segments(&master_playlist, current_variant)
                        .await?;
                }
                DriverState::Seeking(target_time) => {
                    self.seek_to_time(&master_playlist, current_variant, target_time)
                        .await?;
                }
                DriverState::Stopping => {
                    self.state = DriverState::Stopped;
                    self.event_emitter.emit_end_of_stream();
                    break;
                }
                DriverState::Stopped => break,
                _ => {
                    // Invalid state transition
                    return Err(HlsError::Driver("Invalid state transition".to_string()));
                }
            }
        }

        Ok(())
    }

    async fn handle_command(
        &mut self,
        cmd: HlsCommand,
        master_playlist: &MasterPlaylist<'_>,
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
        let selected_variant = variants
            .get(variant)
            .ok_or_else(|| HlsError::VariantNotFound(format!("Variant index {}", variant)))?;

        let media_url = self
            .playlist_manager
            .resolve_url(&self.master_url, &selected_variant.uri())?;
        let media_playlist = self
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
        let selected_variant = variants
            .get(variant)
            .ok_or_else(|| HlsError::VariantNotFound(format!("Variant index {}", variant)))?;

        let media_url = self
            .playlist_manager
            .resolve_url(&self.master_url, &selected_variant.uri())?;
        let media_playlist = self
            .playlist_manager
            .fetch_media_playlist(&media_url)
            .await?;

        // Handle encryption if present - simplified for now
        let key_context = None;

        let mut segment_stream = self.fetch_manager.stream_segment_sequence(
            &media_playlist,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::*;
    use crate::fixture::*;

    #[tokio::test]
    async fn driver_initialization() -> HlsResult<()> {
        let server = TestServer::new().await;
        let (cache, net) = create_test_cache_and_net();

        let master_url = server.url("/master.m3u8")?;
        let options = HlsOptions::default();

        let playlist_manager = PlaylistManager::new(cache.clone(), net.clone(), None);
        let fetch_manager = FetchManager::new(cache.clone(), net.clone());
        let key_manager = KeyManager::new(cache.clone(), net.clone(), None, None, None);
        let abr_config = AbrConfig::default();
        let abr_controller = AbrController::new(abr_config, None, 0);
        let event_emitter = EventEmitter::new();

        let (cmd_sender, cmd_receiver) = mpsc::channel(16);
        let (bytes_sender, _bytes_receiver) = mpsc::channel(100);

        let driver = HlsDriver::new(
            master_url,
            options,
            playlist_manager,
            fetch_manager,
            key_manager,
            abr_controller,
            event_emitter,
            cmd_receiver,
            bytes_sender,
        );

        // Verify driver is created successfully
        assert!(matches!(driver.state, DriverState::Starting));

        Ok(())
    }

    #[tokio::test]
    async fn handle_stop_command() -> HlsResult<()> {
        let server = TestServer::new().await;
        let (cache, net) = create_test_cache_and_net();

        let master_url = server.url("/master.m3u8")?;
        let options = HlsOptions::default();

        let playlist_manager = PlaylistManager::new(cache.clone(), net.clone(), None);
        let fetch_manager = FetchManager::new(cache.clone(), net.clone());
        let key_manager = KeyManager::new(cache.clone(), net.clone(), None, None, None);
        let abr_config = AbrConfig::default();
        let abr_controller = AbrController::new(abr_config, None, 0);
        let event_emitter = EventEmitter::new();

        let (cmd_sender, cmd_receiver) = mpsc::channel(16);
        let (bytes_sender, _bytes_receiver) = mpsc::channel(100);

        let mut driver = HlsDriver::new(
            master_url,
            options,
            playlist_manager,
            fetch_manager,
            key_manager,
            abr_controller,
            event_emitter,
            cmd_receiver,
            bytes_sender,
        );

        // Send stop command
        cmd_sender.send(HlsCommand::Stop).await.unwrap();

        // Handle command
        let master_playlist = driver
            .playlist_manager
            .fetch_master_playlist(&driver.master_url)
            .await?;
        driver
            .handle_command(HlsCommand::Stop, &master_playlist)
            .await?;

        assert!(matches!(driver.state, DriverState::Stopping));

        Ok(())
    }
}
