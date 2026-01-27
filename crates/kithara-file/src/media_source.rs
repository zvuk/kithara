//! File media source implementing kithara_decode::MediaSource.
//!
//! Provides streaming access to progressive audio files for decoding.

use std::{
    io::{self, Read, Seek, SeekFrom},
    sync::Arc,
};

use kithara_assets::{AssetStoreBuilder, asset_root_for_url};
use kithara_decode::{ContainerFormat, MediaInfo, MediaSource, MediaStream};
use kithara_net::HttpClient;
use kithara_storage::StreamingResourceExt;
use kithara_stream::{StreamMsg, Writer};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tracing::{debug, trace};
use url::Url;

use crate::{
    FileEvent, FileParams,
    error::SourceError,
    session::{AssetResourceType, FileStreamState, Progress},
};

/// File media source for streaming decode.
///
/// Implements `MediaSource` trait from kithara-decode.
/// Opens `FileMediaStream` which reads bytes from disk/network.
pub struct FileMediaSource {
    /// File stream state
    state: Arc<FileStreamState>,
    /// Progress tracker
    progress: Arc<Progress>,
    /// Events channel
    events_tx: broadcast::Sender<FileEvent>,
}

impl FileMediaSource {
    /// Open a file media source from URL.
    pub async fn open(url: Url, params: FileParams) -> Result<Self, SourceError> {
        let asset_root = asset_root_for_url(&url);
        let cancel = params.cancel.clone().unwrap_or_default();

        let store = AssetStoreBuilder::new()
            .root_dir(&params.store.cache_dir)
            .asset_root(&asset_root)
            .evict_config(params.store.to_evict_config())
            .cancel(cancel.clone())
            .build();

        let net_client = HttpClient::new(params.net.clone());

        let state = FileStreamState::create(
            Arc::new(store),
            net_client.clone(),
            url,
            cancel.clone(),
            params.event_capacity,
        )
        .await?;

        let (events_tx, _) = broadcast::channel(params.event_capacity);
        let progress = Arc::new(Progress::new());

        // Spawn download writer
        spawn_download_writer(&net_client, state.clone(), progress.clone(), events_tx.clone());

        Ok(Self {
            state,
            progress,
            events_tx,
        })
    }

    /// Subscribe to events.
    pub fn events(&self) -> broadcast::Receiver<FileEvent> {
        self.events_tx.subscribe()
    }

    /// Get file length if known.
    pub fn len(&self) -> Option<u64> {
        self.state.len()
    }

    /// Check if file length is unknown.
    pub fn is_empty(&self) -> bool {
        self.state.len().is_none()
    }
}

impl MediaSource for FileMediaSource {
    fn open(&self) -> io::Result<Box<dyn MediaStream>> {
        // Detect container format from URL
        let container = detect_container_from_url(self.state.url());

        let stream = FileMediaStream::new(
            self.state.res().clone(),
            self.state.len(),
            self.progress.clone(),
            self.events_tx.clone(),
            self.state.cancel().clone(),
            container,
        );

        Ok(Box::new(stream))
    }
}

/// File media stream for reading bytes.
pub struct FileMediaStream {
    /// Resource handle
    res: AssetResourceType,
    /// File length
    len: Option<u64>,
    /// Current read position
    position: u64,
    /// Progress tracker
    progress: Arc<Progress>,
    /// Events channel
    events_tx: broadcast::Sender<FileEvent>,
    /// Cancellation token
    cancel: CancellationToken,
    /// Pending boundary info (set once at start)
    pending_boundary: Option<MediaInfo>,
    /// Whether stream is at EOF
    eof: bool,
}

impl FileMediaStream {
    fn new(
        res: AssetResourceType,
        len: Option<u64>,
        progress: Arc<Progress>,
        events_tx: broadcast::Sender<FileEvent>,
        cancel: CancellationToken,
        container: Option<ContainerFormat>,
    ) -> Self {
        // Set initial boundary with detected container
        let pending_boundary = Some(MediaInfo::new(None, container));

        Self {
            res,
            len,
            position: 0,
            progress,
            events_tx,
            cancel,
            pending_boundary,
            eof: false,
        }
    }

    /// Emit playback progress event.
    fn emit_progress(&self) {
        let percent = self
            .len
            .map(|len| ((self.position as f64 / len as f64) * 100.0).min(100.0) as f32);
        let _ = self.events_tx.send(FileEvent::PlaybackProgress {
            position: self.position,
            percent,
        });
    }
}

impl Read for FileMediaStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.cancel.is_cancelled() {
            debug!("file stream cancelled");
            self.eof = true;
            return Ok(0);
        }

        // Check EOF based on known length
        if let Some(len) = self.len {
            if self.position >= len {
                debug!(position = self.position, len, "EOF: position >= len");
                self.eof = true;
                return Ok(0);
            }
        }

        // Get tokio runtime handle - works from any thread with enter() guard
        let handle = match tokio::runtime::Handle::try_current() {
            Ok(h) => h,
            Err(e) => {
                debug!(error = ?e, "no tokio runtime available");
                return Err(io::Error::new(io::ErrorKind::Other, "no tokio runtime"));
            }
        };

        // Block on async operation
        handle.block_on(async {
            // Calculate how much we want to read
            let want_end = if let Some(len) = self.len {
                (self.position + buf.len() as u64).min(len)
            } else {
                self.position + buf.len() as u64
            };
            let range = self.position..want_end;

            trace!(
                position = self.position,
                want_end,
                buf_len = buf.len(),
                "waiting for range"
            );

            // Wait for data to be available
            match self.res.wait_range(range.clone()).await {
                Ok(kithara_storage::WaitOutcome::Ready) => {
                    trace!(position = self.position, "wait_range returned Ready");
                    // Data is ready, read it
                    match self.res.read_at(self.position, buf).await {
                        Ok(bytes_read) => {
                            trace!(
                                position = self.position,
                                bytes_read, "read_at returned"
                            );
                            if bytes_read == 0 {
                                debug!("read_at returned 0 bytes, setting EOF");
                                self.eof = true;
                            } else {
                                self.position += bytes_read as u64;
                                self.progress.set_read_pos(self.position);
                                self.emit_progress();
                                trace!(position = self.position, bytes_read, "file read");
                            }
                            Ok(bytes_read)
                        }
                        Err(e) => {
                            debug!(error = ?e, "read_at error");
                            Err(io::Error::new(io::ErrorKind::Other, e.to_string()))
                        }
                    }
                }
                Ok(kithara_storage::WaitOutcome::Eof) => {
                    debug!("wait_range returned EOF");
                    // EOF reached
                    self.eof = true;
                    Ok(0)
                }
                Err(e) => {
                    debug!(error = ?e, "wait_range error");
                    Err(io::Error::new(io::ErrorKind::Other, e.to_string()))
                }
            }
        })
    }
}

impl Seek for FileMediaStream {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let new_pos = match pos {
            SeekFrom::Start(p) => p as i64,
            SeekFrom::End(p) => {
                let len = self
                    .len
                    .ok_or_else(|| io::Error::new(io::ErrorKind::Unsupported, "unknown length"))?;
                len as i64 + p
            }
            SeekFrom::Current(p) => self.position as i64 + p,
        };

        if new_pos < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek before start",
            ));
        }

        self.position = new_pos as u64;
        self.eof = false;
        trace!(position = self.position, "file seek");

        Ok(self.position)
    }
}

impl MediaStream for FileMediaStream {
    fn take_boundary(&mut self) -> Option<MediaInfo> {
        self.pending_boundary.take()
    }

    fn is_eof(&self) -> bool {
        self.eof
    }
}

/// Detect container format from URL extension.
fn detect_container_from_url(url: &Url) -> Option<ContainerFormat> {
    let path = url.path();
    let ext = path.rsplit('.').next()?.to_lowercase();

    match ext.as_str() {
        "mp3" => Some(ContainerFormat::MpegAudio),
        "mp4" | "m4a" | "m4b" | "aac" => Some(ContainerFormat::Fmp4),
        "wav" | "wave" => Some(ContainerFormat::Wav),
        "ogg" | "oga" | "opus" => Some(ContainerFormat::Ogg),
        "flac" => None, // Symphonia handles FLAC without container hint
        "ts" => Some(ContainerFormat::MpegTs),
        _ => None,
    }
}

fn spawn_download_writer(
    net_client: &HttpClient,
    state: Arc<FileStreamState>,
    progress: Arc<Progress>,
    events_tx: broadcast::Sender<FileEvent>,
) {
    let net = net_client.clone();
    let url = state.url().clone();
    let len = state.len();
    let res = state.res().clone();
    let cancel = state.cancel().clone();

    tokio::spawn(async move {
        let writer = Writer::<_, _, FileEvent>::new(net, url, None, res, cancel).with_event(
            move |offset, _len| {
                progress.set_download_pos(offset);
                let percent =
                    len.map(|len| ((offset as f64 / len as f64) * 100.0).min(100.0) as f32);
                FileEvent::DownloadProgress { offset, percent }
            },
            move |msg| {
                if let StreamMsg::Event(ev) = msg {
                    let _ = events_tx.send(ev);
                }
            },
        );

        let _ = writer.run_with_fail().await;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_container_mp3() {
        let url = Url::parse("http://example.com/audio.mp3").unwrap();
        assert_eq!(detect_container_from_url(&url), Some(ContainerFormat::MpegAudio));
    }

    #[test]
    fn test_detect_container_m4a() {
        let url = Url::parse("http://example.com/audio.m4a").unwrap();
        assert_eq!(detect_container_from_url(&url), Some(ContainerFormat::Fmp4));
    }

    #[test]
    fn test_detect_container_wav() {
        let url = Url::parse("http://example.com/audio.wav").unwrap();
        assert_eq!(detect_container_from_url(&url), Some(ContainerFormat::Wav));
    }

    #[test]
    fn test_detect_container_ogg() {
        let url = Url::parse("http://example.com/audio.ogg").unwrap();
        assert_eq!(detect_container_from_url(&url), Some(ContainerFormat::Ogg));
    }

    #[test]
    fn test_detect_container_unknown() {
        let url = Url::parse("http://example.com/audio.xyz").unwrap();
        assert_eq!(detect_container_from_url(&url), None);
    }
}
