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
    abr::{AbrController, AbrDecision, AbrReason},
    fetch::FetchManager,
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
    fn from_abr(decision: &AbrDecision, from_variant: usize, current_segment: usize) -> Self {
        Self {
            from: from_variant,
            to: decision.target_variant_index,
            start_segment: if current_segment == usize::MAX {
                0
            } else {
                current_segment.saturating_add(1)
            },
            reason: decision.reason,
        }
    }

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
}

/// Base layer: selects variant, iterates segments, decrypts when needed,
/// responds to commands (seek/force), publishes events.
pub struct BaseStream {
    events: broadcast::Sender<PipelineEvent>,
    cmd_tx: mpsc::Sender<StreamCommand>,
    current_variant: Arc<AtomicUsize>,
    inner: Pin<Box<dyn Stream<Item = PipelineResult<SegmentMeta>> + Send>>,
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
        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let current_variant = abr_controller.current_variant();

        let ctx = StreamContext {
            master_url,
            fetch,
            playlist_manager,
            key_manager,
            abr: abr_controller,
            cancel,
        };

        let inner = create_stream(ctx, events.clone(), cmd_rx);

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
        let _ = self
            .cmd_tx
            .try_send(StreamCommand::ForceVariant { variant_index, from });
    }

    pub fn current_variant(&self) -> usize {
        self.current_variant.load(Ordering::SeqCst)
    }
}

impl Stream for BaseStream {
    type Item = PipelineResult<SegmentMeta>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.get_mut().inner.as_mut().poll_next(cx)
    }
}

impl PipelineStream for BaseStream {
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
            StreamCommand::ForceVariant { variant_index, from } => {
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

    Ok(VariantContext { media_url, playlist })
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
        StreamCommand::ForceVariant { variant_index, from } => {
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

async fn prepare_segment_meta(
    fetch: &FetchManager,
    segment: &MediaSegment,
    ctx: &VariantContext,
    variant: usize,
    idx: usize,
    key_manager: &Option<Arc<KeyManager>>,
    events: &broadcast::Sender<PipelineEvent>,
) -> HlsResult<SegmentMeta> {
    let segment_url = ctx
        .media_url
        .join(&segment.uri)
        .map_err(|e| HlsError::InvalidUrl(format!("Failed to resolve segment URL: {}", e)))?;

    // Wait for segment to be fully downloaded (no bytes in memory).
    let fetch_meta = fetch.download_streaming(&segment_url).await?;

    let duration = fetch_meta.duration.max(Duration::from_millis(1));
    let meta = build_segment_meta(segment, &ctx.media_url, variant, idx, duration, fetch_meta.len)?;

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

    Ok(meta)
}

// --- Main stream creation ---

fn create_stream(
    ctx: StreamContext,
    events: broadcast::Sender<PipelineEvent>,
    mut cmd_rx: mpsc::Receiver<StreamCommand>,
) -> Pin<Box<dyn Stream<Item = PipelineResult<SegmentMeta>> + Send>> {
    Box::pin(stream! {
        let StreamContext { master_url, fetch, playlist_manager, key_manager, mut abr, cancel } = ctx;
        let initial_variant = abr.current_variant().load(Ordering::Acquire);
        let mut state = IterState::new(initial_variant);

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

            let ctx = match load_variant_context(&playlist_manager, &master_url, state.to).await {
                Ok(c) => c,
                Err(e) => {
                    yield Err(PipelineError::Hls(e));
                    return;
                }
            };

            // Yield init segment if needed (variant switch or first segment)
            let need_init = ctx.playlist.init_segment.is_some()
                && (state.from != state.to || state.start_segment == 0);
            if need_init {
                match fetch_init_segment(&fetch, &ctx, state.to).await {
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

            // Iterate segments (no bytes in memory - data stays on disk).
            let mut switch: Option<SwitchDecision> = None;

            for idx in state.start_segment..ctx.playlist.segments.len() {
                if cancel.is_cancelled() {
                    return;
                }

                let segment = match ctx.playlist.segments.get(idx) {
                    Some(s) => s,
                    None => {
                        yield Err(PipelineError::Hls(HlsError::SegmentNotFound(
                            format!("Index {}", idx)
                        )));
                        break;
                    }
                };

                // Prepare segment meta (waits for download, no bytes in memory).
                match prepare_segment_meta(&fetch, segment, &ctx, state.to, idx, &key_manager, &events).await {
                    Ok(meta) => {
                        // Check ABR using segment length from disk.
                        let duration = meta.duration.unwrap_or(Duration::from_millis(1));
                        if let Some((from, to, reason)) = abr.process_fetch(meta.len as usize, duration, &variants) {
                            let _ = events.send(PipelineEvent::VariantApplied { from, to, reason });
                            let decision = AbrDecision {
                                target_variant_index: to,
                                reason,
                                changed: true,
                            };
                            switch = Some(SwitchDecision::from_abr(&decision, from, idx));
                        }

                        let _ = events.send(PipelineEvent::SegmentReady {
                            variant: state.to,
                            segment_index: idx,
                        });
                        yield Ok(meta);
                    }
                    Err(e) => {
                        yield Err(PipelineError::Hls(e));
                        break;
                    }
                }

                // Check commands after yield
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
