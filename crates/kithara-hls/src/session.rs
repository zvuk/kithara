#![forbid(unsafe_code)]

//! `kithara-stream::io::Source` adapter for HLS.
//!
//! HlsSessionSource wraps HlsDriver.stream() and provides random-access via wait_range/read_at.
//! Data flows: driver.stream() → internal buffer → wait_range/read_at → SyncReader → decoder.

use std::ops::Range;

use async_trait::async_trait;
use bytes::{Bytes, BytesMut};
use futures::StreamExt;
use kithara_storage::WaitOutcome;
use kithara_stream::{Source, StreamError as KitharaIoError, StreamResult as KitharaIoResult};
use std::sync::Arc;
use tokio::sync::{Notify, RwLock, broadcast};

use crate::{HlsError, driver::HlsDriver, events::HlsEvent, options::HlsOptions, playlist::MasterPlaylist};

/// Selects the effective variant index to use for this session.
pub fn select_variant_index(master: &MasterPlaylist, options: &HlsOptions) -> usize {
    if let Some(selector) = &options.variant_stream_selector {
        selector(master).unwrap_or_else(|| options.abr_initial_variant_index.unwrap_or(0))
    } else {
        options.abr_initial_variant_index.unwrap_or(0)
    }
}

/// Internal buffer state for random-access over streaming data.
struct BufferState {
    /// Accumulated bytes from driver.stream().
    data: BytesMut,
    /// True when stream has ended (no more data coming).
    finished: bool,
    /// Error message if stream failed.
    error: Option<String>,
}

impl BufferState {
    fn new() -> Self {
        Self {
            data: BytesMut::new(),
            finished: false,
            error: None,
        }
    }
}

/// Source adapter that reads from HlsDriver.stream() and provides random-access.
pub struct HlsSessionSource {
    buffer: Arc<RwLock<BufferState>>,
    notify: Arc<Notify>,
}

impl HlsSessionSource {
    /// Create a new source that reads from the given driver's stream.
    pub fn new(driver: HlsDriver) -> Self {
        let buffer = Arc::new(RwLock::new(BufferState::new()));
        let notify = Arc::new(Notify::new());

        // Spawn background task to read from driver.stream() into buffer.
        let buffer_clone = Arc::clone(&buffer);
        let notify_clone = Arc::clone(&notify);

        tokio::spawn(async move {
            let mut stream = Box::pin(driver.stream());

            while let Some(result) = stream.next().await {
                match result {
                    Ok(bytes) => {
                        let mut state = buffer_clone.write().await;
                        state.data.extend_from_slice(&bytes);
                        drop(state);
                        notify_clone.notify_waiters();
                    }
                    Err(e) => {
                        let mut state = buffer_clone.write().await;
                        state.error = Some(e.to_string());
                        state.finished = true;
                        drop(state);
                        notify_clone.notify_waiters();
                        return;
                    }
                }
            }

            // Stream ended normally.
            let mut state = buffer_clone.write().await;
            state.finished = true;
            drop(state);
            notify_clone.notify_waiters();
        });

        Self { buffer, notify }
    }
}

#[async_trait]
impl Source for HlsSessionSource {
    type Error = HlsError;

    async fn wait_range(&self, range: Range<u64>) -> KitharaIoResult<WaitOutcome, HlsError> {
        loop {
            {
                let state = self.buffer.read().await;

                // Check for error.
                if let Some(ref e) = state.error {
                    return Err(KitharaIoError::Source(HlsError::Driver(e.clone())));
                }

                let available = state.data.len() as u64;

                // If we have enough data, return Ready.
                if range.start < available {
                    return Ok(WaitOutcome::Ready);
                }

                // If stream finished and we don't have enough data, it's EOF.
                if state.finished {
                    return Ok(WaitOutcome::Eof);
                }
            }

            // Wait for more data.
            self.notify.notified().await;
        }
    }

    async fn read_at(&self, offset: u64, len: usize) -> KitharaIoResult<Bytes, HlsError> {
        let state = self.buffer.read().await;

        // Check for error.
        if let Some(ref e) = state.error {
            return Err(KitharaIoError::Source(HlsError::Driver(e.clone())));
        }

        let available = state.data.len() as u64;

        // If offset is at or past available data, return empty.
        if offset >= available {
            return Ok(Bytes::new());
        }

        let start = offset as usize;
        let end = (offset + len as u64).min(available) as usize;
        let bytes = Bytes::copy_from_slice(&state.data[start..end]);

        Ok(bytes)
    }

    fn len(&self) -> Option<u64> {
        // Length is unknown for streaming HLS.
        None
    }
}

// =============================================================================
// HlsSession - main entry point
// =============================================================================

pub struct HlsSession {
    driver: HlsDriver,
}

impl HlsSession {
    pub(crate) fn new(driver: HlsDriver) -> Self {
        Self { driver }
    }

    /// Get event receiver for stream events.
    pub fn events(&self) -> broadcast::Receiver<HlsEvent> {
        self.driver.events()
    }

    /// Create a Source for random-access reading (used with SyncReader for rodio).
    pub fn source(self) -> HlsSessionSource {
        HlsSessionSource::new(self.driver)
    }
}
