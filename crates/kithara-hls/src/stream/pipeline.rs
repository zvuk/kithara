//! Main stream creation logic (async generator).

use std::{pin::Pin, sync::atomic::Ordering, time::Instant};

use async_stream::stream;
use futures::Stream;
use tokio::sync::{broadcast, mpsc};

use super::{
    commands::{IterState, StreamCommand, SwitchDecision, process_commands},
    context::{StreamContext, build_segment_meta, fetch_init_segment, load_variant_context},
    types::{PipelineError, PipelineEvent, PipelineResult, SegmentMeta},
};
use crate::{
    HlsError,
    abr::{ThroughputSample, ThroughputSampleSource},
    fetch::ActiveFetchResult,
};

/// Minimum bytes to accumulate before pushing a throughput sample.
const MIN_SAMPLE_BYTES: u64 = 32_000; // 32KB

/// Create the main segment stream.
pub fn create_stream(
    ctx: StreamContext,
    events: broadcast::Sender<PipelineEvent>,
    mut cmd_rx: mpsc::Receiver<StreamCommand>,
) -> Pin<Box<dyn Stream<Item = PipelineResult<SegmentMeta>> + Send>> {
    Box::pin(stream! {
        let StreamContext {
            master_url,
            fetch,
            playlist_manager,
            _key_manager: _,
            mut abr,
            cancel,
        } = ctx;

        let initial_variant = abr.current_variant().load(Ordering::Acquire);
        let mut state = IterState::new(initial_variant);

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
                    yield Err(PipelineError::Hls(e));
                    return;
                }
            };

            if state.to >= variants.len() {
                yield Err(PipelineError::Hls(HlsError::VariantNotFound(format!(
                    "variant {}",
                    state.to
                ))));
                return;
            }

            // Load variant context first, then emit event only on success.
            let var_ctx =
                match load_variant_context(&playlist_manager, &master_url, state.to).await {
                    Ok(c) => c,
                    Err(e) => {
                        yield Err(PipelineError::Hls(e));
                        return;
                    }
                };

            // Emit event AFTER successful variant load.
            let variant_switched = state.from != state.to;
            if variant_switched {
                let _ = events.send(PipelineEvent::VariantApplied {
                    from: state.from,
                    to: state.to,
                    reason: state.reason,
                });
                state.from = state.to;
            }

            // Yield init segment if needed (variant switch or first segment).
            let need_init = var_ctx.playlist.init_segment.is_some()
                && (variant_switched || state.start_segment == 0);
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

            // Iterate segments with streaming download and direct ABR updates.
            let mut switch: Option<SwitchDecision> = None;

            for idx in state.start_segment..var_ctx.playlist.segments.len() {
                if cancel.is_cancelled() {
                    return;
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

                // Build segment URL.
                let segment_url = match var_ctx.media_url.join(&segment.uri) {
                    Ok(url) => url,
                    Err(e) => {
                        yield Err(PipelineError::Hls(HlsError::InvalidUrl(format!(
                            "Failed to resolve segment URL: {}",
                            e
                        ))));
                        break;
                    }
                };

                // Start timing BEFORE fetch (includes connection time, server delays).
                let fetch_start = Instant::now();

                // Start fetch (or get cached info).
                let fetch_result = match fetch.start_fetch(&segment_url).await {
                    Ok(r) => r,
                    Err(e) => {
                        yield Err(PipelineError::Hls(e));
                        break;
                    }
                };

                // Determine segment length and drive fetch with ABR updates.
                let segment_len = match fetch_result {
                    ActiveFetchResult::Cached { bytes } => bytes,
                    ActiveFetchResult::Active(mut active_fetch) => {
                        let mut accumulated_bytes = 0u64;
                        let mut last_report_at = fetch_start;

                        // Iterate over chunks, update ABR directly.
                        loop {
                            match active_fetch.next_chunk().await {
                                Ok(Some(chunk_bytes)) => {
                                    accumulated_bytes += chunk_bytes;

                                    // Push throughput sample when accumulated enough.
                                    if accumulated_bytes >= MIN_SAMPLE_BYTES {
                                        let now = Instant::now();
                                        let dur = now.duration_since(last_report_at);
                                        let sample = ThroughputSample {
                                            bytes: accumulated_bytes,
                                            duration: dur,
                                            at: now,
                                            source: ThroughputSampleSource::Network,
                                        };
                                        tracing::debug!(
                                            bytes = accumulated_bytes,
                                            duration_ms = dur.as_millis(),
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
                                            let _ = events.send(PipelineEvent::VariantApplied {
                                                from,
                                                to: decision.target_variant_index,
                                                reason: decision.reason,
                                            });
                                            switch = Some(SwitchDecision {
                                                from,
                                                to: decision.target_variant_index,
                                                start_segment: idx.saturating_add(1),
                                                reason: decision.reason,
                                            });
                                        }
                                    }
                                }
                                Ok(None) => break, // Fetch complete
                                Err(e) => {
                                    yield Err(PipelineError::Hls(e));
                                    return;
                                }
                            }
                        }

                        // Push remaining accumulated bytes.
                        if accumulated_bytes > 0 {
                            let now = Instant::now();
                            let sample = ThroughputSample {
                                bytes: accumulated_bytes,
                                duration: now.duration_since(last_report_at),
                                at: now,
                                source: ThroughputSampleSource::Network,
                            };
                            abr.push_throughput_sample(sample);
                        }

                        active_fetch.commit().await
                    }
                };

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
                        yield Err(PipelineError::Hls(e));
                        break;
                    }
                };

                // Update buffer: segment downloaded and ready.
                buffered_secs += segment.duration.as_secs_f64();

                let _ = events.send(PipelineEvent::SegmentReady {
                    variant: state.to,
                    segment_index: idx,
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

            if let Some(sw) = switch {
                state.apply_switch(&sw);
                continue;
            }

            break;
        }
    })
}
