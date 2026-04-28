//! Event-throttling cache + helpers for `record_bandwidth` / `tick`.

use kithara_events::{AbrEvent, EventBus};
use kithara_platform::time::{Duration, Instant};
use num_traits::ToPrimitive;

use super::{core::AbrController, peer::PeerEntry};

/// Per-peer throttling state for sample / estimate / buffer events.
#[derive(Default)]
pub(super) struct EventThrottleCache {
    pub(super) last_bandwidth_emit: Option<(Instant, u64)>,
    pub(super) last_buffer_emit: Option<(Instant, Option<Duration>)>,
    pub(super) last_throughput_sample_at: Option<Instant>,
}

impl AbrController {
    pub(super) fn emit_throttled(
        &self,
        entry: &PeerEntry,
        bus: &Option<EventBus>,
        now: Instant,
        estimate_bps: Option<u64>,
        buffer_ahead: Option<Duration>,
    ) {
        let Some(bus) = bus else {
            return;
        };
        let mut throttle = entry.throttle.lock_sync();

        if let Some(bps) = estimate_bps {
            let should_emit = match throttle.last_bandwidth_emit {
                None => true,
                Some((t, prev)) => {
                    let time_ok =
                        now.duration_since(t) >= self.settings.bandwidth_emit_min_interval;
                    let delta_ok =
                        relative_delta(prev, bps) >= self.settings.bandwidth_emit_min_delta_ratio;
                    time_ok || delta_ok
                }
            };
            if should_emit {
                throttle.last_bandwidth_emit = Some((now, bps));
                bus.publish(AbrEvent::BandwidthEstimate { bps });
            }
        }

        let should_emit_buffer = match throttle.last_buffer_emit {
            None => true,
            Some((t, prev)) => {
                let time_ok = now.duration_since(t) >= self.settings.buffer_emit_min_interval;
                let transition = prev.is_some() != buffer_ahead.is_some();
                let delta_ok = match (prev, buffer_ahead) {
                    (Some(a), Some(b)) => {
                        duration_delta(a, b) >= self.settings.buffer_emit_min_delta
                    }
                    _ => true,
                };
                transition || (time_ok && delta_ok)
            }
        };
        if should_emit_buffer {
            throttle.last_buffer_emit = Some((now, buffer_ahead));
            drop(throttle);
            bus.publish(AbrEvent::BufferAhead {
                ahead: buffer_ahead,
            });
        }
    }
}

pub(super) fn bytes_per_second(bytes: u64, duration: Duration) -> f64 {
    let secs = duration.as_secs_f64().max(f64::EPSILON);
    let bytes_f = bytes.to_f64().unwrap_or(0.0);
    bytes_f / secs
}

fn duration_delta(a: Duration, b: Duration) -> Duration {
    a.abs_diff(b)
}

fn relative_delta(prev: u64, now: u64) -> f64 {
    if prev == 0 {
        return f64::INFINITY;
    }
    let diff = (i128::from(prev) - i128::from(now))
        .unsigned_abs()
        .to_f64()
        .unwrap_or(f64::INFINITY);
    let base = prev.to_f64().unwrap_or(f64::INFINITY);
    diff / base
}
