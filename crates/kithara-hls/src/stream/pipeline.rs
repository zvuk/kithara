//! Main stream creation logic (async generator).

use std::{pin::Pin, sync::atomic::Ordering};

use async_stream::stream;
use futures::Stream;
use tokio::sync::{broadcast, mpsc};

use super::{
    commands::{IterState, StreamCommand, process_commands},
    context::{
        StreamContext, drive_download_and_report, fetch_init_segment, load_variant_context,
        prepare_segment_streaming, process_throughput_samples,
    },
    types::{PipelineError, PipelineEvent, PipelineResult, SegmentMeta},
};
use crate::{HlsError, abr::ThroughputSample};

/// Create the main segment stream.
pub fn create_stream(
    ctx: StreamContext,
    events: broadcast::Sender<PipelineEvent>,
    mut cmd_rx: mpsc::Receiver<StreamCommand>,
    throughput_tx: mpsc::Sender<ThroughputSample>,
) -> Pin<Box<dyn Stream<Item = PipelineResult<SegmentMeta>> + Send>> {
    Box::pin(stream! {
        let StreamContext {
            master_url,
            fetch,
            playlist_manager,
            key_manager,
            mut abr,
            cancel,
            mut throughput_rx,
        } = ctx;

        let initial_variant = abr.current_variant().load(Ordering::Acquire);
        let mut state = IterState::new(initial_variant);
        let mut last_segment_index: usize = usize::MAX;

        loop {
            if cancel.is_cancelled() {
                return;
            }

            // Process all pending commands (process_all = true).
            process_commands(&mut cmd_rx, &mut state, &mut abr, true);

            let variants = match playlist_manager.variants(&master_url).await {
                Ok(v) => v,
                Err(e) => {
                    yield Err(PipelineError::Hls(e));
                    return;
                }
            };

            // Process any pending throughput samples.
            if let Some(sw) = process_throughput_samples(
                &mut throughput_rx,
                &mut abr,
                &variants,
                &events,
                last_segment_index,
            ) {
                state.apply_switch(&sw);
                continue;
            }

            if state.to >= variants.len() {
                yield Err(PipelineError::Hls(HlsError::VariantNotFound(format!(
                    "variant {}",
                    state.to
                ))));
                return;
            }

            if state.from != state.to {
                let _ = events.send(PipelineEvent::VariantApplied {
                    from: state.from,
                    to: state.to,
                    reason: state.reason,
                });
            }

            let var_ctx =
                match load_variant_context(&playlist_manager, &master_url, state.to).await {
                    Ok(c) => c,
                    Err(e) => {
                        yield Err(PipelineError::Hls(e));
                        return;
                    }
                };

            // Yield init segment if needed (variant switch or first segment).
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
            let mut switch = None;

            for idx in state.start_segment..var_ctx.playlist.segments.len() {
                if cancel.is_cancelled() {
                    return;
                }

                // Process throughput samples before each segment.
                if let Some(sw) = process_throughput_samples(
                    &mut throughput_rx,
                    &mut abr,
                    &variants,
                    &events,
                    last_segment_index,
                ) {
                    switch = Some(sw);
                    break;
                }

                let segment = match var_ctx.playlist.segments.get(idx) {
                    Some(s) => s,
                    None => {
                        yield Err(PipelineError::Hls(HlsError::SegmentNotFound(format!(
                            "Index {}",
                            idx
                        ))));
                        break;
                    }
                };

                // Prepare segment (get expected length, don't wait for download).
                match prepare_segment_streaming(
                    &fetch,
                    segment,
                    &var_ctx,
                    state.to,
                    idx,
                    &key_manager,
                    &events,
                )
                .await
                {
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
                if let Some(sw) = process_throughput_samples(
                    &mut throughput_rx,
                    &mut abr,
                    &variants,
                    &events,
                    last_segment_index,
                ) {
                    switch = Some(sw);
                    break;
                }

                // Check commands after yield (process_all = false, single command).
                if let Some(sw) = process_commands(&mut cmd_rx, &mut state, &mut abr, false) {
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
