//! `record_bandwidth` + `tick` — drives one peer's decision cycle.

use std::sync::atomic::Ordering;

use kithara_events::{AbrEvent, AbrReason, BandwidthSource, VariantInfo};
use kithara_platform::time::{Duration, Instant};

use super::{
    core::{AbrController, AbrPeerId},
    peer::PeerEntry,
    throttle::bytes_per_second,
};
use crate::state::AbrView;

impl AbrController {
    /// Record a bandwidth sample for `peer_id`. Called by the Downloader
    /// when a fetch completes. Also triggers a `tick` for the peer.
    pub fn record_bandwidth(
        &self,
        peer_id: AbrPeerId,
        bytes: u64,
        fetch_duration: Duration,
        source: BandwidthSource,
    ) {
        if fetch_duration.as_millis() < self.settings.min_throughput_record_ms {
            return;
        }
        self.estimator.push_sample(bytes, fetch_duration, source);

        let Some(entry) = self.peer_entry(peer_id) else {
            return;
        };

        let prev = entry.bytes_downloaded.fetch_add(bytes, Ordering::AcqRel);
        let now_total = prev.saturating_add(bytes);

        let now = Instant::now();
        let bus = entry.bus();
        if let Some(ref bus) = bus {
            let mut throttle = entry.throttle.lock_sync();
            let emit = throttle
                .last_throughput_sample_at
                .is_none_or(|t| now.duration_since(t) >= Self::MIN_THROUGHPUT_SAMPLE_INTERVAL);
            if emit {
                throttle.last_throughput_sample_at = Some(now);
                drop(throttle);
                let bps = bytes_per_second(bytes, fetch_duration);
                bus.publish(AbrEvent::ThroughputSample {
                    source,
                    bytes_per_second: bps,
                });
            }
        }

        if !entry.warmup_completed.load(Ordering::Acquire)
            && now_total >= self.settings.warmup_min_bytes
            && entry
                .warmup_completed
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            && let Some(ref bus) = bus
        {
            bus.publish(AbrEvent::WarmupCompleted);
        }

        self.tick(peer_id, now);
    }

    pub(super) fn tick(&self, peer_id: AbrPeerId, now: Instant) {
        let Some(ctx) = TickContext::resolve(self, peer_id) else {
            return;
        };

        let bus = ctx.entry.bus();
        let variants = ctx.peer.variants();
        let progress = ctx.peer.progress();
        let buffer_ahead = progress.map(|p| {
            p.download_head_playback_time
                .saturating_sub(p.reader_playback_time)
        });
        let bytes_downloaded = ctx.entry.bytes_downloaded.load(Ordering::Acquire);
        let estimate_bps = self.estimator.estimate_bps();

        if !ctx
            .entry
            .variants_registered_published
            .load(Ordering::Acquire)
            && let Some(ref bus) = bus
        {
            let infos: Vec<VariantInfo> = variants
                .iter()
                .map(|v| VariantInfo {
                    index: v.variant_index,
                    bandwidth_bps: Some(v.bandwidth_bps),
                    name: None,
                    codecs: None,
                    container: None,
                })
                .collect();
            let initial = ctx
                .entry
                .state
                .as_ref()
                .map_or(0, |s| s.current_variant_index());
            bus.publish(AbrEvent::VariantsRegistered {
                initial,
                variants: infos,
            });
            ctx.entry
                .variants_registered_published
                .store(true, Ordering::Release);
        }

        self.emit_throttled(&ctx.entry, &bus, now, estimate_bps, buffer_ahead);

        let Some(state) = ctx.entry.state.as_ref() else {
            return;
        };

        let view = AbrView {
            buffer_ahead,
            estimate_bps,
            bytes_downloaded,
            settings: &self.settings,
            variants: &variants,
        };
        let decision = state.decide(&view, now);

        if decision.changed {
            let current_before = state.current_variant_index();
            state.apply(&decision, now);
            if let Some(ref bus) = bus {
                bus.publish(AbrEvent::VariantApplied {
                    from: current_before,
                    to: decision.target_variant_index,
                    reason: decision.reason,
                });
            }
            let reader_pt = progress.map_or(Duration::ZERO, |p| p.reader_playback_time);
            self.schedule_incoherence_watch(peer_id, reader_pt, now);
        } else if decision.reason != AbrReason::AlreadyOptimal
            && let Some(ref bus) = bus
        {
            bus.publish(AbrEvent::DecisionSkipped {
                reason: decision.reason,
            });
        }
    }
}

/// Resolved peer context for one tick — collapses the previous let-else
/// cascade in `tick()` into one `?` chain. Returning `None` from any of the
/// three lookups is "abort silently"; the lint distinguishes this from the
/// heterogeneous cascade case in `decide()` and a single Option-resolver
/// is the recommended fix.
struct TickContext {
    entry: std::sync::Arc<PeerEntry>,
    peer: std::sync::Arc<dyn crate::abr::Abr>,
}

impl TickContext {
    fn resolve(controller: &AbrController, peer_id: AbrPeerId) -> Option<Self> {
        let entry = controller.peer_entry(peer_id)?;
        let peer = entry.peer_weak.upgrade()?;
        Some(Self { entry, peer })
    }
}
