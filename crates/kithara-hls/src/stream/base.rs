use std::{
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};

use async_stream::stream;
use futures::Stream;
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;
use url::Url;

use super::types::{PipelineError, PipelineEvent, PipelineResult, PipelineStream, SegmentMeta};
use crate::{
    HlsError, HlsResult,
    abr::{AbrController, AbrReason, ThroughputSample},
    fetch::{DownloadContext, FetchManager},
    keys::KeyManager,
    playlist::{MediaPlaylist, MediaSegment, PlaylistManager, VariantId},
};

/// Commands for stream control.
#[derive(Debug)]
enum StreamCommand {
    Seek { segment_index: usize },
    ForceVariant { variant_index: usize, from: usize },
}

/// Segment iteration state.
#[derive(Clone)]
struct IterState {
    from: usize,
    to: usize,
    start_segment: usize,
    reason: AbrReason,
}

impl IterState {
    fn new(variant: usize) -> Self {
        Self {
            from: variant,
            to: variant,
            start_segment: 0,
            reason: AbrReason::Initial,
        }
    }

    fn apply_seek(&mut self, segment_index: usize) {
        self.start_segment = segment_index;
    }

    fn apply_force_variant(&mut self, variant_index: usize, from: usize) {
        self.from = from;
        self.to = variant_index;
        self.start_segment = 0;
        self.reason = AbrReason::ManualOverride;
    }

    fn apply_switch(&mut self, next: &SwitchDecision) {
        self.from = next.from;
        self.to = next.to;
        self.start_segment = next.start_segment;
        self.reason = next.reason;
    }
}

/// Variant switch decision.
struct SwitchDecision {
    from: usize,
    to: usize,
    start_segment: usize,
    reason: AbrReason,
}

impl SwitchDecision {
    fn from_seek(current_to: usize, segment_index: usize) -> Self {
        Self {
            from: current_to,
            to: current_to,
            start_segment: segment_index,
            reason: AbrReason::ManualOverride,
        }
    }

    fn from_force(variant_index: usize, from: usize) -> Self {
        Self {
            from,
            to: variant_index,
            start_segment: 0,
            reason: AbrReason::ManualOverride,
        }
    }
}

/// Loaded variant context.
struct VariantContext {
    media_url: Url,
    playlist: MediaPlaylist,
}

/// Stream creation context.
struct StreamContext {
    master_url: Url,
    fetch: Arc<FetchManager>,
    playlist_manager: Arc<PlaylistManager>,
    key_manager: Option<Arc<KeyManager>>,
    abr: AbrController,
    cancel: CancellationToken,
    throughput_rx: mpsc::Receiver<ThroughputSample>,
}

/// Base layer: selects variant, iterates segments, decrypts when needed,
/// responds to commands (seek/force), publishes events.
pub struct SegmentStream {
    events: broadcast::Sender<PipelineEvent>,
    cmd_tx: mpsc::Sender<StreamCommand>,
    current_variant: Arc<AtomicUsize>,
    inner: Pin<Box<dyn Stream<Item = PipelineResult<SegmentMeta>> + Send>>,
}

impl SegmentStream {
    pub fn new(
        master_url: Url,
        fetch: Arc<FetchManager>,
        playlist_manager: Arc<PlaylistManager>,
        key_manager: Option<Arc<KeyManager>>,
        abr_controller: AbrController,
        cancel: CancellationToken,
    ) -> Self {
        let (events, _) = broadcast::channel(32);
        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (throughput_tx, throughput_rx) = mpsc::channel(32);
        let current_variant = abr_controller.current_variant();

        let ctx = StreamContext {
            master_url,
            fetch,
            playlist_manager,
            key_manager,
            abr: abr_controller,
            cancel,
            throughput_rx,
        };

        let inner = create_stream(ctx, events.clone(), cmd_rx, throughput_tx);

        Self {
            events,
            cmd_tx,
            current_variant,
            inner,
        }
    }

    pub fn seek(&self, segment_index: usize) {
        let _ = self.cmd_tx.try_send(StreamCommand::Seek { segment_index });
    }

    pub fn force_variant(&self, variant_index: usize) {
        let from = self.current_variant.swap(variant_index, Ordering::SeqCst);
        let _ = self.events.send(PipelineEvent::VariantApplied {
            from,
            to: variant_index,
            reason: AbrReason::ManualOverride,
        });
        let _ = self.cmd_tx.try_send(StreamCommand::ForceVariant {
            variant_index,
            from,
        });
    }

    pub fn current_variant(&self) -> usize {
        self.current_variant.load(Ordering::SeqCst)
    }
}

impl Stream for SegmentStream {
    type Item = PipelineResult<SegmentMeta>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.get_mut().inner.as_mut().poll_next(cx)
    }
}

impl PipelineStream for SegmentStream {
    fn event_sender(&self) -> broadcast::Sender<PipelineEvent> {
        self.events.clone()
    }
}

// --- Helper functions ---

fn process_commands(
    cmd_rx: &mut mpsc::Receiver<StreamCommand>,
    state: &mut IterState,
    abr: &mut AbrController,
) {
    while let Ok(cmd) = cmd_rx.try_recv() {
        match cmd {
            StreamCommand::Seek { segment_index } => {
                state.apply_seek(segment_index);
            }
            StreamCommand::ForceVariant {
                variant_index,
                from,
            } => {
                state.apply_force_variant(variant_index, from);
                abr.set_current_variant(variant_index);
            }
        }
    }
}

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

fn handle_post_yield_command(
    cmd_rx: &mut mpsc::Receiver<StreamCommand>,
    abr: &mut AbrController,
    current_to: usize,
) -> Option<SwitchDecision> {
    let Ok(cmd) = cmd_rx.try_recv() else {
        return None;
    };

    match cmd {
        StreamCommand::Seek { segment_index } => {
            Some(SwitchDecision::from_seek(current_to, segment_index))
        }
        StreamCommand::ForceVariant {
            variant_index,
            from,
        } => {
            abr.set_current_variant(variant_index);
            Some(SwitchDecision::from_force(variant_index, from))
        }
    }
}

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

    let fetch_meta = fetch.download_streaming(&init_url).await?;

    Ok(SegmentMeta {
        variant,
        segment_index: usize::MAX,
        sequence: ctx.playlist.media_sequence,
        url: init_url,
        duration: Some(fetch_meta.duration),
        key: init.key.clone(),
        len: fetch_meta.len,
    })
}

/// Result of streaming segment preparation.
struct StreamingSegmentResult {
    meta: SegmentMeta,
    download: Option<DownloadContext>,
}

async fn prepare_segment_streaming(
    fetch: &FetchManager,
    segment: &MediaSegment,
    ctx: &VariantContext,
    variant: usize,
    idx: usize,
    key_manager: &Option<Arc<KeyManager>>,
    events: &broadcast::Sender<PipelineEvent>,
) -> HlsResult<StreamingSegmentResult> {
    let segment_url = ctx
        .media_url
        .join(&segment.uri)
        .map_err(|e| HlsError::InvalidUrl(format!("Failed to resolve segment URL: {}", e)))?;

    // Get expected length and download context (no waiting for full download).
    let prepare_result = fetch.prepare_streaming(&segment_url).await?;
    let expected_len = prepare_result.expected_len.unwrap_or(0);

    // Use segment duration from playlist, not download duration.
    let duration = segment.duration;
    let meta = build_segment_meta(
        segment,
        &ctx.media_url,
        variant,
        idx,
        duration,
        expected_len,
    )?;

    // Check if decryption is needed.
    let needs_decryption = key_manager.is_some() && meta.key.is_some();
    if needs_decryption {
        // TODO: Implement disk-based decryption (read → decrypt → write to .dec file).
        return Err(HlsError::Unimplemented);
    }

    if meta.key.is_some() {
        let _ = events.send(PipelineEvent::Decrypted {
            variant: meta.variant,
            segment_index: meta.segment_index,
        });
    }

    Ok(StreamingSegmentResult {
        meta,
        download: prepare_result.download,
    })
}

/// Process pending throughput samples from background downloads.
fn process_throughput_samples(
    throughput_rx: &mut mpsc::Receiver<ThroughputSample>,
    abr: &mut AbrController,
    variants: &[crate::abr::Variant],
    events: &broadcast::Sender<PipelineEvent>,
    last_segment_index: usize,
) -> Option<SwitchDecision> {
    let mut switch = None;
    while let Ok(sample) = throughput_rx.try_recv() {
        let duration = sample.duration;
        abr.push_throughput_sample(sample);

        // Check for variant switch based on new throughput data.
        let decision = abr.decide(variants, duration.as_secs_f64(), std::time::Instant::now());
        if decision.changed {
            let from = abr.current_variant().load(Ordering::Acquire);
            abr.apply(&decision, std::time::Instant::now());
            let _ = events.send(PipelineEvent::VariantApplied {
                from,
                to: decision.target_variant_index,
                reason: decision.reason,
            });
            // Continue from next segment, not from 0.
            let next_segment = if last_segment_index == usize::MAX {
                0
            } else {
                last_segment_index.saturating_add(1)
            };
            switch = Some(SwitchDecision {
                from,
                to: decision.target_variant_index,
                start_segment: next_segment,
                reason: decision.reason,
            });
        }
    }
    switch
}

/// Drive download and send throughput sample.
async fn drive_download_and_report(
    fetch: &FetchManager,
    download_ctx: DownloadContext,
    throughput_tx: &mpsc::Sender<ThroughputSample>,
) {
    let sample = fetch.drive_download(download_ctx).await;
    let _ = throughput_tx.send(sample).await;
}

// --- Main stream creation ---

fn create_stream(
    ctx: StreamContext,
    events: broadcast::Sender<PipelineEvent>,
    mut cmd_rx: mpsc::Receiver<StreamCommand>,
    throughput_tx: mpsc::Sender<ThroughputSample>,
) -> Pin<Box<dyn Stream<Item = PipelineResult<SegmentMeta>> + Send>> {
    Box::pin(stream! {
        let StreamContext { master_url, fetch, playlist_manager, key_manager, mut abr, cancel, mut throughput_rx } = ctx;
        let initial_variant = abr.current_variant().load(Ordering::Acquire);
        let mut state = IterState::new(initial_variant);
        let mut last_segment_index: usize = usize::MAX;

        loop {
            if cancel.is_cancelled() {
                return;
            }

            process_commands(&mut cmd_rx, &mut state, &mut abr);

            let variants = match playlist_manager.variants(&master_url).await {
                Ok(v) => v,
                Err(e) => {
                    yield Err(PipelineError::Hls(e));
                    return;
                }
            };

            // Process any pending throughput samples.
            if let Some(sw) = process_throughput_samples(&mut throughput_rx, &mut abr, &variants, &events, last_segment_index) {
                state.apply_switch(&sw);
                continue;
            }

            if state.to >= variants.len() {
                yield Err(PipelineError::Hls(HlsError::VariantNotFound(
                    format!("variant {}", state.to)
                )));
                return;
            }

            if state.from != state.to {
                let _ = events.send(PipelineEvent::VariantApplied {
                    from: state.from,
                    to: state.to,
                    reason: state.reason,
                });
            }

            let var_ctx = match load_variant_context(&playlist_manager, &master_url, state.to).await {
                Ok(c) => c,
                Err(e) => {
                    yield Err(PipelineError::Hls(e));
                    return;
                }
            };

            // Yield init segment if needed (variant switch or first segment)
            let need_init = var_ctx.playlist.init_segment.is_some()
                && (state.from != state.to || state.start_segment == 0);
            if need_init {
                match fetch_init_segment(&fetch, &var_ctx, state.to).await {
                    Ok(payload) => {
                        let _ = events.send(PipelineEvent::SegmentReady {
                            variant: state.to,
                            segment_index: usize::MAX,
                        });
                        yield Ok(payload);
                    }
                    Err(e) => {
                        yield Err(PipelineError::Hls(e));
                        return;
                    }
                }
            }

            // Iterate segments with streaming (yield early, then wait for download).
            let mut switch: Option<SwitchDecision> = None;

            for idx in state.start_segment..var_ctx.playlist.segments.len() {
                if cancel.is_cancelled() {
                    return;
                }

                // Process throughput samples before each segment.
                if let Some(sw) = process_throughput_samples(&mut throughput_rx, &mut abr, &variants, &events, last_segment_index) {
                    switch = Some(sw);
                    break;
                }

                let segment = match var_ctx.playlist.segments.get(idx) {
                    Some(s) => s,
                    None => {
                        yield Err(PipelineError::Hls(HlsError::SegmentNotFound(
                            format!("Index {}", idx)
                        )));
                        break;
                    }
                };

                // Prepare segment (get expected length, don't wait for download).
                match prepare_segment_streaming(&fetch, segment, &var_ctx, state.to, idx, &key_manager, &events).await {
                    Ok(result) => {
                        let _ = events.send(PipelineEvent::SegmentReady {
                            variant: state.to,
                            segment_index: idx,
                        });

                        // Yield meta early (consumer can start using it).
                        yield Ok(result.meta);
                        last_segment_index = idx;

                        // Drive download to completion and report throughput to ABR.
                        if let Some(download_ctx) = result.download {
                            drive_download_and_report(&fetch, download_ctx, &throughput_tx).await;
                        }
                    }
                    Err(e) => {
                        yield Err(PipelineError::Hls(e));
                        break;
                    }
                }

                // Process throughput samples after download.
                if let Some(sw) = process_throughput_samples(&mut throughput_rx, &mut abr, &variants, &events, last_segment_index) {
                    switch = Some(sw);
                    break;
                }

                // Check commands after yield.
                if let Some(sw) = handle_post_yield_command(&mut cmd_rx, &mut abr, state.to) {
                    switch = Some(sw);
                }

                if switch.is_some() {
                    break;
                }
            }

            if let Some(sw) = switch {
                state.apply_switch(&sw);
                continue;
            }

            break;
        }
    })
}
