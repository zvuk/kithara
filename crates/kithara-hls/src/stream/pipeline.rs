//! SegmentStream: variant selection, segment iteration, ABR, and commands.

use std::{
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll},
    time::{Duration, Instant},
};

use async_stream::stream;
use futures::Stream;
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;
use url::Url;

use super::types::{PipelineError, PipelineResult, SegmentMeta, StreamCommand, VariantSwitch};
use crate::{
    HlsError, HlsResult,
    abr::{DefaultAbrController, ThroughputSample, ThroughputSampleSource},
    events::HlsEvent,
    fetch::{ActiveFetchResult, FetchManager},
    keys::KeyManager,
    playlist::{MediaPlaylist, MediaSegment, PlaylistManager, VariantId},
};

/// Parameters for creating a [`SegmentStream`].
pub struct SegmentStreamParams {
    pub master_url: Url,
    pub fetch: Arc<FetchManager>,
    pub playlist_manager: Arc<PlaylistManager>,
    pub key_manager: Option<Arc<KeyManager>>,
    pub abr_controller: DefaultAbrController,
    pub events_tx: broadcast::Sender<HlsEvent>,
    pub cancel: CancellationToken,
    pub command_capacity: usize,
    /// Minimum bytes to accumulate before pushing a throughput sample.
    pub min_sample_bytes: u64,
}

/// Sentinel value meaning "no next variant".
const NO_NEXT_VARIANT: usize = usize::MAX;

/// Handle for controlling the pipeline from HlsSource.
/// Can be cloned and used independently from the stream.
#[derive(Clone)]
pub struct PipelineHandle {
    cmd_tx: mpsc::Sender<StreamCommand>,
    /// Variant currently being read by decoder.
    current_variant: Arc<AtomicUsize>,
    /// Next variant to switch to (set by ABR), or NO_NEXT_VARIANT if none.
    next_variant: Arc<AtomicUsize>,
}

impl PipelineHandle {
    /// Seek to a specific segment index.
    /// If ABR switch is pending, commits the switch first.
    pub fn seek(&self, segment_index: usize) {
        // Commit pending ABR switch on seek.
        self.commit_variant_switch();
        let _ = self.cmd_tx.try_send(StreamCommand::Seek { segment_index });
    }

    /// Force switch to a specific variant.
    pub fn force_variant(&self, variant_index: usize) {
        let from = self.current_variant.load(Ordering::SeqCst);
        let _ = self.cmd_tx.try_send(StreamCommand::ForceVariant {
            variant_index,
            from,
        });
    }

    /// Get current variant index (variant being read).
    pub fn current_variant(&self) -> usize {
        self.current_variant.load(Ordering::SeqCst)
    }

    /// Directly set current_variant (for finished playlists where pipeline is stopped).
    /// WARNING: Only use when playlist is finished and pipeline won't process commands!
    pub(crate) fn set_current_variant_direct(&self, variant: usize) {
        self.current_variant.store(variant, Ordering::SeqCst);
    }

    /// Get next variant if ABR requested a switch, or None.
    pub fn next_variant(&self) -> Option<usize> {
        let v = self.next_variant.load(Ordering::SeqCst);
        if v == NO_NEXT_VARIANT { None } else { Some(v) }
    }

    /// Set next variant (called by ABR when it decides to switch).
    pub fn set_next_variant(&self, variant: usize) {
        self.next_variant.store(variant, Ordering::SeqCst);
    }

    /// Commit switch: current = next, next = None.
    /// Called by adapter when it's ready to switch.
    pub fn commit_variant_switch(&self) {
        let next = self.next_variant.load(Ordering::SeqCst);
        if next != NO_NEXT_VARIANT {
            self.current_variant.store(next, Ordering::SeqCst);
            self.next_variant.store(NO_NEXT_VARIANT, Ordering::SeqCst);
            tracing::info!(
                from = self.current_variant.load(Ordering::SeqCst),
                to = next,
                "Committed variant switch"
            );
        }
    }
}

/// Base layer: selects variant, iterates segments, decrypts when needed,
/// responds to commands (seek/force), publishes events.
pub struct SegmentStream {
    inner: Pin<Box<dyn Stream<Item = PipelineResult<SegmentMeta>> + Send>>,
}

impl SegmentStream {
    /// Create a new segment stream.
    /// Returns a handle for controlling the pipeline and the stream itself.
    pub fn new(params: SegmentStreamParams) -> (PipelineHandle, Self) {
        let (cmd_tx, cmd_rx) = mpsc::channel(params.command_capacity);
        // Get initial variant from ABR, but create independent atomic for reading.
        // ABR's current_variant = target for download.
        // Handle's current_variant = what decoder is reading.
        let initial_variant = params
            .abr_controller
            .current_variant()
            .load(Ordering::Acquire);

        let next_variant = Arc::new(AtomicUsize::new(NO_NEXT_VARIANT));

        let handle = PipelineHandle {
            cmd_tx,
            current_variant: Arc::new(AtomicUsize::new(initial_variant)),
            next_variant: Arc::clone(&next_variant),
        };

        let inner = create_stream(
            params.master_url,
            params.fetch,
            params.playlist_manager,
            params.key_manager,
            params.abr_controller,
            params.cancel,
            params.events_tx,
            cmd_rx,
            params.min_sample_bytes,
            next_variant,
        );

        (handle, Self { inner })
    }
}

impl Stream for SegmentStream {
    type Item = PipelineResult<SegmentMeta>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.get_mut().inner.as_mut().poll_next(cx)
    }
}

// ============================================================================
// Variant context and helpers
// ============================================================================

/// Loaded variant context.
struct VariantContext {
    media_url: Url,
    playlist: MediaPlaylist,
}

/// Load variant context (master + media playlist).
async fn load_variant_context(
    playlist_manager: &PlaylistManager,
    master_url: &Url,
    variant_index: usize,
) -> HlsResult<VariantContext> {
    let master = playlist_manager.master_playlist(master_url).await?;

    let variant = master
        .variants
        .get(variant_index)
        .ok_or_else(|| HlsError::VariantNotFound(format!("variant {}", variant_index)))?;

    let media_url = playlist_manager.resolve_url(master_url, &variant.uri)?;
    let playlist = playlist_manager
        .media_playlist(&media_url, VariantId(variant_index))
        .await?;

    Ok(VariantContext {
        media_url,
        playlist,
    })
}

/// Build segment metadata from playlist segment.
fn build_segment_meta(
    segment: &MediaSegment,
    media_url: &Url,
    variant: usize,
    idx: usize,
    duration: Duration,
    len: u64,
) -> HlsResult<SegmentMeta> {
    let segment_url = media_url
        .join(&segment.uri)
        .map_err(|e| HlsError::InvalidUrl(format!("Failed to resolve segment URL: {}", e)))?;

    Ok(SegmentMeta {
        variant,
        segment_index: idx,
        sequence: segment.sequence,
        url: segment_url,
        duration: Some(duration),
        key: segment.key.clone(),
        len,
    })
}

/// Fetch init segment for variant.
async fn fetch_init_segment(
    fetch: &FetchManager,
    ctx: &VariantContext,
    variant: usize,
) -> HlsResult<SegmentMeta> {
    let init = ctx
        .playlist
        .init_segment
        .as_ref()
        .ok_or_else(|| HlsError::SegmentNotFound("no init segment".to_string()))?;

    let init_url = ctx
        .media_url
        .join(&init.uri)
        .map_err(|e| HlsError::InvalidUrl(format!("Failed to resolve init URL: {}", e)))?;

    let fetch_result = fetch.fetch_init(&init_url).await?;

    Ok(SegmentMeta {
        variant,
        segment_index: usize::MAX,
        sequence: ctx.playlist.media_sequence,
        url: init_url,
        duration: Some(fetch_result.duration),
        key: init.key.clone(),
        len: fetch_result.bytes,
    })
}

// ============================================================================
// Command processing
// ============================================================================

/// Process pending commands from the command channel.
/// Returns new state if a switch is needed.
fn process_commands(
    cmd_rx: &mut mpsc::Receiver<StreamCommand>,
    state: &mut VariantSwitch,
    abr: &mut DefaultAbrController,
    process_all: bool,
) -> Option<VariantSwitch> {
    let mut result = None;

    while let Ok(cmd) = cmd_rx.try_recv() {
        tracing::debug!(
            ?cmd,
            process_all,
            state_start_segment = state.start_segment,
            "Processing command"
        );
        match cmd {
            StreamCommand::Seek { segment_index } => {
                if process_all {
                    state.apply_seek(segment_index);
                    tracing::debug!(
                        segment_index,
                        state_start_segment = state.start_segment,
                        "Applied seek (process_all)"
                    );
                } else {
                    result = Some(VariantSwitch::with_seek(state.to, segment_index));
                }
            }
            StreamCommand::ForceVariant {
                variant_index,
                from,
            } => {
                abr.set_current_variant(variant_index);
                if process_all {
                    state.apply_force_variant(variant_index, from);
                } else {
                    result = Some(VariantSwitch::with_force_variant(variant_index, from));
                }
            }
        }

        if !process_all {
            break;
        }
    }

    result
}

// ============================================================================
// Stream creation (async generator)
// ============================================================================

/// Create the main segment stream.
#[allow(clippy::too_many_arguments)]
fn create_stream(
    master_url: Url,
    fetch: Arc<FetchManager>,
    playlist_manager: Arc<PlaylistManager>,
    _key_manager: Option<Arc<KeyManager>>,
    mut abr: DefaultAbrController,
    cancel: CancellationToken,
    events: broadcast::Sender<HlsEvent>,
    mut cmd_rx: mpsc::Receiver<StreamCommand>,
    min_sample_bytes: u64,
    next_variant: Arc<AtomicUsize>,
) -> Pin<Box<dyn Stream<Item = PipelineResult<SegmentMeta>> + Send>> {
    Box::pin(stream! {
        let initial_variant = abr.current_variant().load(Ordering::Acquire);
        let mut state = VariantSwitch::new(initial_variant);

        // Buffer tracking: assume real-time playback consumption.
        let playback_start = Instant::now();
        let mut buffered_secs: f64 = 0.0;

        loop {
            if cancel.is_cancelled() {
                return;
            }

            // Process all pending commands (process_all = true).
            process_commands(&mut cmd_rx, &mut state, &mut abr, true);

            let variants = match playlist_manager.variants(&master_url).await {
                Ok(v) => v,
                Err(e) => {
                    let _ = events.send(HlsEvent::Error {
                        error: e.to_string(),
                        recoverable: false,
                    });
                    yield Err(PipelineError::Hls(e));
                    return;
                }
            };

            if state.to >= variants.len() {
                let err = HlsError::VariantNotFound(format!("variant {}", state.to));
                let _ = events.send(HlsEvent::Error {
                    error: err.to_string(),
                    recoverable: false,
                });
                yield Err(PipelineError::Hls(err));
                return;
            }

            // Load variant context first, then emit event only on success.
            let var_ctx =
                match load_variant_context(&playlist_manager, &master_url, state.to).await {
                    Ok(c) => c,
                    Err(e) => {
                        let _ = events.send(HlsEvent::Error {
                            error: e.to_string(),
                            recoverable: false,
                        });
                        yield Err(PipelineError::Hls(e));
                        return;
                    }
                };

            tracing::debug!(
                state_start_segment = state.start_segment,
                variant = state.to,
                "After variant context load"
            );

            // Emit event AFTER successful variant load.
            let variant_switched = state.from != state.to;
            if variant_switched {
                let _ = events.send(HlsEvent::VariantApplied {
                    from_variant: state.from,
                    to_variant: state.to,
                    reason: state.reason,
                });
                state.from = state.to;
            }

            // Yield init segment if needed (variant switch or first segment).
            let need_init = var_ctx.playlist.init_segment.is_some()
                && (variant_switched || state.start_segment == 0);
            tracing::debug!(
                need_init,
                variant_switched,
                state_start_segment = state.start_segment,
                "Init segment check"
            );
            if need_init {
                match fetch_init_segment(&fetch, &var_ctx, state.to).await {
                    Ok(payload) => {
                        let _ = events.send(HlsEvent::SegmentStart {
                            variant: state.to,
                            segment_index: usize::MAX,
                            byte_offset: 0,
                        });
                        yield Ok(payload);
                    }
                    Err(e) => {
                        let _ = events.send(HlsEvent::Error {
                            error: e.to_string(),
                            recoverable: false,
                        });
                        yield Err(PipelineError::Hls(e));
                        return;
                    }
                }
            }

            // Iterate segments with streaming download and direct ABR updates.
            let mut switch: Option<VariantSwitch> = None;

            tracing::debug!(
                start_segment = state.start_segment,
                segments_len = var_ctx.playlist.segments.len(),
                variant = state.to,
                "Starting segment iteration"
            );

            for idx in state.start_segment..var_ctx.playlist.segments.len() {
                if cancel.is_cancelled() {
                    return;
                }

                let segment = match var_ctx.playlist.segments.get(idx) {
                    Some(s) => s,
                    None => {
                        let err = HlsError::SegmentNotFound(format!("Index {}", idx));
                        let _ = events.send(HlsEvent::Error {
                            error: err.to_string(),
                            recoverable: false,
                        });
                        yield Err(PipelineError::Hls(err));
                        break;
                    }
                };

                // Build segment URL.
                let segment_url = match var_ctx.media_url.join(&segment.uri) {
                    Ok(url) => url,
                    Err(e) => {
                        let err = HlsError::InvalidUrl(format!("Failed to resolve segment URL: {}", e));
                        let _ = events.send(HlsEvent::Error {
                            error: err.to_string(),
                            recoverable: false,
                        });
                        yield Err(PipelineError::Hls(err));
                        break;
                    }
                };

                // Start timing BEFORE fetch (includes connection time, server delays).
                let fetch_start = Instant::now();

                // Start fetch (or get cached info).
                let fetch_result = match fetch.start_fetch(&segment_url).await {
                    Ok(r) => r,
                    Err(e) => {
                        let _ = events.send(HlsEvent::Error {
                            error: e.to_string(),
                            recoverable: true, // Network errors may be retryable
                        });
                        yield Err(PipelineError::Hls(e));
                        break;
                    }
                };

                // Track total bytes for this segment.
                let mut total_segment_bytes = 0u64;

                // Determine segment length and drive fetch with ABR updates.
                let segment_len = match fetch_result {
                    ActiveFetchResult::Cached { bytes } => {
                        total_segment_bytes = bytes;
                        bytes
                    }
                    ActiveFetchResult::Active(mut active_fetch) => {
                        let mut accumulated_bytes = 0u64;
                        let mut last_report_at = fetch_start;

                        // Iterate over chunks, update ABR directly.
                        loop {
                            match active_fetch.next_chunk().await {
                                Ok(Some(chunk_bytes)) => {
                                    accumulated_bytes += chunk_bytes;
                                    total_segment_bytes += chunk_bytes;

                                    // Emit download progress.
                                    let _ = events.send(HlsEvent::DownloadProgress {
                                        offset: total_segment_bytes,
                                        percent: None, // Per-segment progress, no total percent
                                    });

                                    // Push throughput sample when accumulated enough.
                                    if accumulated_bytes >= min_sample_bytes {
                                        let now = Instant::now();
                                        let dur = now.duration_since(last_report_at);
                                        let bps = if dur.as_secs_f64() > 0.0 {
                                            accumulated_bytes as f64 / dur.as_secs_f64()
                                        } else {
                                            0.0
                                        };

                                        // Emit throughput sample event.
                                        let _ = events.send(HlsEvent::ThroughputSample {
                                            bytes_per_second: bps,
                                        });

                                        let sample = ThroughputSample {
                                            bytes: accumulated_bytes,
                                            duration: dur,
                                            at: now,
                                            source: ThroughputSampleSource::Network,
                                        };
                                        tracing::debug!(
                                            bytes = accumulated_bytes,
                                            duration_ms = dur.as_millis(),
                                            bps = format!("{:.0}", bps),
                                            "ABR: pushing throughput sample"
                                        );
                                        abr.push_throughput_sample(sample);
                                        accumulated_bytes = 0;
                                        last_report_at = now;

                                        // Skip ABR decisions in Manual mode.
                                        if !abr.is_auto() {
                                            continue;
                                        }

                                        // Check if ABR wants to switch variant.
                                        // Buffer = downloaded duration - elapsed playback time.
                                        let elapsed = playback_start.elapsed().as_secs_f64();
                                        let buffer_level = (buffered_secs - elapsed).max(0.0);
                                        let decision = abr.decide(&variants, buffer_level, now);
                                        tracing::debug!(
                                            target = decision.target_variant_index,
                                            reason = ?decision.reason,
                                            changed = decision.changed,
                                            buffer_level_secs = format!("{:.1}", buffer_level),
                                            "ABR: decide result"
                                        );
                                        if decision.changed {
                                            let from = abr.current_variant().load(Ordering::Acquire);
                                            abr.apply(&decision, now);
                                            // Set next_variant for deferred switch.
                                            // Decoder will commit switch when ready.
                                            next_variant.store(decision.target_variant_index, Ordering::SeqCst);
                                            let _ = events.send(HlsEvent::VariantApplied {
                                                from_variant: from,
                                                to_variant: decision.target_variant_index,
                                                reason: decision.reason,
                                            });
                                            switch = Some(VariantSwitch::with_abr_switch(
                                                from,
                                                decision.target_variant_index,
                                                idx.saturating_add(1),
                                                decision.reason,
                                            ));
                                        }
                                    }
                                }
                                Ok(None) => break, // Fetch complete
                                Err(e) => {
                                    let _ = events.send(HlsEvent::Error {
                                        error: e.to_string(),
                                        recoverable: false,
                                    });
                                    yield Err(PipelineError::Hls(e));
                                    return;
                                }
                            }
                        }

                        // Push remaining accumulated bytes.
                        if accumulated_bytes > 0 {
                            let now = Instant::now();
                            let dur = now.duration_since(last_report_at);
                            let bps = if dur.as_secs_f64() > 0.0 {
                                accumulated_bytes as f64 / dur.as_secs_f64()
                            } else {
                                0.0
                            };
                            let _ = events.send(HlsEvent::ThroughputSample {
                                bytes_per_second: bps,
                            });
                            let sample = ThroughputSample {
                                bytes: accumulated_bytes,
                                duration: dur,
                                at: now,
                                source: ThroughputSampleSource::Network,
                            };
                            abr.push_throughput_sample(sample);
                        }

                        active_fetch.commit().await
                    }
                };

                // Emit segment complete event.
                let fetch_duration = fetch_start.elapsed();
                let _ = events.send(HlsEvent::SegmentComplete {
                    variant: state.to,
                    segment_index: idx,
                    bytes_transferred: total_segment_bytes,
                    duration: fetch_duration,
                });

                // Build and yield segment meta.
                let meta = match build_segment_meta(
                    segment,
                    &var_ctx.media_url,
                    state.to,
                    idx,
                    segment.duration,
                    segment_len,
                ) {
                    Ok(m) => m,
                    Err(e) => {
                        let _ = events.send(HlsEvent::Error {
                            error: e.to_string(),
                            recoverable: false,
                        });
                        yield Err(PipelineError::Hls(e));
                        break;
                    }
                };

                // Update buffer: segment downloaded and ready.
                buffered_secs += segment.duration.as_secs_f64();

                // Emit buffer level (available buffer = downloaded - played).
                let elapsed = playback_start.elapsed().as_secs_f64();
                let buffer_level = (buffered_secs - elapsed).max(0.0) as f32;
                let _ = events.send(HlsEvent::BufferLevel {
                    level_seconds: buffer_level,
                });

                let _ = events.send(HlsEvent::SegmentStart {
                    variant: state.to,
                    segment_index: idx,
                    byte_offset: 0,
                });
                yield Ok(meta);

                // Check commands after segment (process_all = false, single command).
                if let Some(sw) = process_commands(&mut cmd_rx, &mut state, &mut abr, false) {
                    switch = Some(sw);
                }

                if switch.is_some() {
                    break;
                }
            }

            tracing::debug!(
                switch_pending = switch.is_some(),
                last_segment = state.start_segment,
                variant = state.to,
                "Segment loop ended"
            );

            if let Some(sw) = switch {
                tracing::debug!(
                    from = sw.from,
                    to = sw.to,
                    start_segment = sw.start_segment,
                    "Continuing with variant switch"
                );
                state = sw;
                tracing::debug!(
                    state_start_segment = state.start_segment,
                    state_to = state.to,
                    "State after assignment"
                );
                continue;
            }

            tracing::debug!(variant = state.to, "No more segments, ending stream");
            break;
        }
    })
}
