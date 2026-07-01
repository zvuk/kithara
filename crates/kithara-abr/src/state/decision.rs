use kithara_events::{AbrMode, AbrReason, VariantIndex, VariantInfo};
use kithara_platform::time::Instant;
use kithara_test_utils::probe::IntoProbeArg;
use num_traits::ToPrimitive;

use super::{core::AbrState, view::AbrView};
use crate::controller::AbrSettings;

/// Outcome of an ABR decision step.
///
/// Replaces the historical flat `{ reason, did_change, target_variant_index }`
/// struct: the variant itself encodes whether and how the index changes, so
/// an inconsistent `did_change` / target pairing is unrepresentable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum AbrDecision {
    /// No switch — stay on `current`; `reason` records why.
    Stay {
        current: VariantIndex,
        reason: AbrReason,
    },
    /// Bandwidth-driven up-switch.
    UpSwitch {
        from: VariantIndex,
        to: VariantIndex,
        reason: AbrReason,
    },
    /// Bandwidth- or buffer-driven down-switch.
    DownSwitch {
        from: VariantIndex,
        to: VariantIndex,
        reason: AbrReason,
    },
    /// User-selected fixed variant (ABR disabled).
    Manual {
        from: VariantIndex,
        to: VariantIndex,
    },
}

impl AbrDecision {
    /// `true` when the decision moves off the current variant.
    #[must_use]
    pub fn changed(&self) -> bool {
        !matches!(self, Self::Stay { .. })
    }

    /// The reason recorded for this decision.
    #[must_use]
    pub fn reason(&self) -> AbrReason {
        self.into()
    }

    /// The variant this decision lands on (`current` for [`Self::Stay`]).
    #[must_use]
    pub fn target(&self) -> VariantIndex {
        self.into()
    }
}

impl From<&AbrDecision> for VariantIndex {
    fn from(decision: &AbrDecision) -> Self {
        match decision {
            AbrDecision::Stay { current, .. } => *current,
            AbrDecision::UpSwitch { to, .. }
            | AbrDecision::DownSwitch { to, .. }
            | AbrDecision::Manual { to, .. } => *to,
        }
    }
}

impl From<&AbrDecision> for AbrReason {
    fn from(decision: &AbrDecision) -> Self {
        match decision {
            AbrDecision::Stay { reason, .. }
            | AbrDecision::UpSwitch { reason, .. }
            | AbrDecision::DownSwitch { reason, .. } => *reason,
            AbrDecision::Manual { .. } => Self::ManualOverride,
        }
    }
}

impl IntoProbeArg for &AbrDecision {
    fn into_probe_arg(self) -> u64 {
        self.target().get().to_u64().unwrap_or(0)
    }
}

/// Produce a decision without mutating state.
///
/// Phase 1: parallel computes. None of the `let X = expr` lines branches —
/// the compiler can reorder/coalesce these atomic reads, and the CPU
/// schedules them in parallel.
///
/// Phase 2: a single tuple-match dispatches to the early-exit outcome or
/// extracts the bandwidth estimate for the actual decision logic. One
/// statement, one decision site — jump-table eligible.
///
/// Phase 3: bandwidth-aware switch logic (`up_switch` / `down_switch`)
/// runs on the gates' shared output; no further early returns.
pub(crate) fn evaluate(state: &AbrState, view: &AbrView<'_>, now: Instant) -> AbrDecision {
    let current = state.current_variant_index();

    let locked = state.is_locked();
    let manual_target = match state.mode() {
        AbrMode::Manual(idx) => Some(idx),
        AbrMode::Auto(_) => None,
    };
    let cant_switch = !state.can_switch_now(now, view.settings.min_switch_interval);
    let estimate_bps = view.estimate_bps;

    let estimate_bps: u64 = match (locked, manual_target, cant_switch, estimate_bps) {
        (true, _, _, _) => {
            return AbrDecision::Stay {
                current,
                reason: AbrReason::Locked,
            };
        }
        (_, Some(idx), _, _) => return manual_decision(current, idx),
        (_, _, true, _) => {
            return AbrDecision::Stay {
                current,
                reason: AbrReason::MinInterval,
            };
        }
        (_, _, _, None) => {
            return AbrDecision::Stay {
                current,
                reason: AbrReason::NoEstimate,
            };
        }
        (false, None, false, Some(bps)) => bps,
    };

    let escaping = state.is_escaping();
    let max_bw = state.max_bandwidth_bps();
    let mut sorted = sorted_candidates(view.variants, max_bw);
    if escaping {
        // The active variant cannot deliver — it must not be a Stay target.
        sorted.retain(|(idx, _)| *idx != current);
    }
    if sorted.is_empty() {
        return AbrDecision::Stay {
            current,
            reason: AbrReason::AlreadyOptimal,
        };
    }

    let current_bw = current_bandwidth(&sorted, current);
    let adjusted_bps = adjusted_throughput(estimate_bps, view.settings.throughput_safety_factor);

    let Some((candidate_idx, candidate_bw)) = candidate_variant(&sorted, adjusted_bps) else {
        return AbrDecision::Stay {
            current,
            reason: AbrReason::AlreadyOptimal,
        };
    };

    if escaping {
        // `current` was excluded from `sorted` above, so `candidate_idx !=
        // current`. Escape to the bandwidth-viable candidate, bypassing the
        // buffer-too-low and hysteresis gates: their premise (staying grows the
        // buffer) is false for a variant that delivers nothing.
        return AbrDecision::UpSwitch {
            from: current,
            to: candidate_idx,
            reason: AbrReason::EscapeStalled,
        };
    }

    if let Some(cap) = max_bw
        && current_bw > cap
        && candidate_idx != current
    {
        return AbrDecision::DownSwitch {
            from: current,
            to: candidate_idx,
            reason: AbrReason::DownSwitch,
        };
    }

    let ctx = SwitchContext {
        adjusted_bps,
        candidate_bw,
        candidate_idx,
        current,
        current_bw,
        buffer_ahead: view.buffer_ahead,
        settings: view.settings,
    };

    if candidate_bw > current_bw {
        return up_switch(ctx);
    }
    if candidate_bw < current_bw
        && let Some(d) = down_switch(ctx)
    {
        return d;
    }

    AbrDecision::Stay {
        current,
        reason: AbrReason::AlreadyOptimal,
    }
}

fn manual_decision(current: VariantIndex, idx: VariantIndex) -> AbrDecision {
    if idx == current {
        AbrDecision::Stay {
            current,
            reason: AbrReason::ManualOverride,
        }
    } else {
        AbrDecision::Manual {
            from: current,
            to: idx,
        }
    }
}

fn sorted_candidates(variants: &[VariantInfo], max_bw: Option<u64>) -> Vec<(VariantIndex, u64)> {
    let mut out: Vec<(VariantIndex, u64)> = variants
        .iter()
        .map(|v| (v.variant_index, v.bandwidth_bps.unwrap_or(0)))
        .filter(|(_, bw)| max_bw.is_none_or(|cap| *bw <= cap))
        .collect();
    out.sort_by_key(|(_, bw)| *bw);
    out
}

fn current_bandwidth(sorted: &[(VariantIndex, u64)], current: VariantIndex) -> u64 {
    sorted
        .iter()
        .find(|(idx, _)| *idx == current)
        .map_or(0, |(_, bw)| *bw)
}

fn adjusted_throughput(estimate_bps: u64, safety_factor: f64) -> f64 {
    let raw = estimate_bps.to_f64().unwrap_or(0.0);
    (raw / safety_factor).max(0.0)
}

fn candidate_variant(
    sorted: &[(VariantIndex, u64)],
    adjusted_bps: f64,
) -> Option<(VariantIndex, u64)> {
    let best_under = sorted
        .iter()
        .filter(|(_, bw)| bw.to_f64().unwrap_or(f64::INFINITY) <= adjusted_bps)
        .max_by_key(|(_, bw)| *bw);
    let lowest = sorted.first();
    best_under.or(lowest).map(|(idx, bw)| (*idx, *bw))
}

#[derive(Clone, Copy)]
struct SwitchContext<'a> {
    settings: &'a AbrSettings,
    buffer_ahead: Option<kithara_platform::time::Duration>,
    candidate_idx: VariantIndex,
    current: VariantIndex,
    adjusted_bps: f64,
    candidate_bw: u64,
    current_bw: u64,
}

fn up_switch(ctx: SwitchContext<'_>) -> AbrDecision {
    let buffer_ok = ctx
        .buffer_ahead
        .is_none_or(|b| b >= ctx.settings.min_buffer_for_up_switch);
    let candidate_bw_f = ctx.candidate_bw.to_f64().unwrap_or(f64::INFINITY);
    let headroom_ok = ctx.adjusted_bps >= candidate_bw_f * ctx.settings.up_hysteresis_ratio;
    if buffer_ok && headroom_ok {
        return AbrDecision::UpSwitch {
            from: ctx.current,
            to: ctx.candidate_idx,
            reason: AbrReason::UpSwitch,
        };
    }
    AbrDecision::Stay {
        current: ctx.current,
        reason: AbrReason::BufferTooLowForUpSwitch,
    }
}

fn down_switch(ctx: SwitchContext<'_>) -> Option<AbrDecision> {
    let urgent = ctx
        .buffer_ahead
        .is_some_and(|b| b <= ctx.settings.urgent_downswitch_buffer);
    let current_bw_f = ctx.current_bw.to_f64().unwrap_or(f64::INFINITY);
    let margin_ok = ctx.adjusted_bps <= current_bw_f * ctx.settings.down_hysteresis_ratio;
    if urgent {
        return Some(AbrDecision::DownSwitch {
            from: ctx.current,
            to: ctx.candidate_idx,
            reason: AbrReason::UrgentDownSwitch,
        });
    }
    if margin_ok {
        return Some(AbrDecision::DownSwitch {
            from: ctx.current,
            to: ctx.candidate_idx,
            reason: AbrReason::DownSwitch,
        });
    }
    None
}
