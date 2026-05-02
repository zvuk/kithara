//! Pure decision logic for ABR variant switching.
//!
//! [`evaluate`] is the entry point — it gathers inputs in parallel, then
//! dispatches via a single tuple match. Heterogeneous-cascade `if`s with
//! distinct early-return reasons are an anti-pattern (CPU branch
//! predictor cannot fold them into a jump table). The match collapses
//! the dispatch into one site that the compiler can lay out as branchless
//! / jump-table code while preserving per-gate `AbrReason` attribution
//! for telemetry. See `xtask/src/idioms/checks/guard_cascade.rs` module
//! docs for the rationale.

use kithara_events::{AbrMode, AbrReason, AbrVariant};
use kithara_platform::time::Instant;
use num_traits::ToPrimitive;

use super::{core::AbrState, view::AbrView};
use crate::controller::AbrSettings;

/// Outcome of an ABR decision step.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AbrDecision {
    pub reason: AbrReason,
    pub changed: bool,
    pub target_variant_index: usize,
}

/// Produce a decision without mutating state.
///
/// Phase 1: parallel computes. None of the `let X = expr` lines branches —
/// the compiler can reorder/coalesce these atomic reads, and the CPU
/// schedules them in parallel.
///
/// Phase 2: a single tuple-match dispatches to the early-exit reason or
/// extracts the bandwidth estimate for the actual decision logic. One
/// statement, one decision site — jump-table eligible.
///
/// Phase 3: bandwidth-aware switch logic (`up_switch` / `down_switch`)
/// runs on the gates' shared output; no further early returns.
pub(crate) fn evaluate(state: &AbrState, view: &AbrView<'_>, now: Instant) -> AbrDecision {
    let current = state.current_variant_index();

    // Phase 1: parallel compute (pure `let`-statements, no control flow).
    let locked = state.is_locked();
    let manual_target = match state.mode() {
        AbrMode::Manual(idx) => Some(idx),
        AbrMode::Auto(_) => None,
    };
    let cant_switch = !state.can_switch_now(now, view.settings.min_switch_interval);
    let warming = view.bytes_downloaded < view.settings.warmup_min_bytes;
    let estimate_bps = view.estimate_bps;

    // Phase 2: one tuple match — replaces the heterogeneous guard cascade.
    let estimate_bps: u64 = match (locked, manual_target, cant_switch, warming, estimate_bps) {
        (true, _, _, _, _) => return decision(current, current, AbrReason::Locked),
        (_, Some(idx), _, _, _) => return decision(current, idx, AbrReason::ManualOverride),
        (_, _, true, _, _) => return decision(current, current, AbrReason::MinInterval),
        (_, _, _, true, _) => return decision(current, current, AbrReason::Warmup),
        (_, _, _, _, None) => return decision(current, current, AbrReason::NoEstimate),
        (false, None, false, false, Some(bps)) => bps,
    };

    // Phase 3: bandwidth-aware decision over `(current, estimate_bps)`.
    let max_bw = state.max_bandwidth_bps();
    let sorted = sorted_candidates(view.variants, max_bw);
    if sorted.is_empty() {
        return decision(current, current, AbrReason::AlreadyOptimal);
    }

    let current_bw = current_bandwidth(&sorted, current);
    let adjusted_bps = adjusted_throughput(estimate_bps, view.settings.throughput_safety_factor);

    let Some((candidate_idx, candidate_bw)) = candidate_variant(&sorted, adjusted_bps) else {
        return decision(current, current, AbrReason::AlreadyOptimal);
    };

    // Forced down-switch when current exceeds cap.
    if let Some(cap) = max_bw
        && current_bw > cap
        && candidate_idx != current
    {
        return decision(current, candidate_idx, AbrReason::DownSwitch);
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

    decision(current, current, AbrReason::AlreadyOptimal)
}

pub(super) fn decision(current: usize, target: usize, reason: AbrReason) -> AbrDecision {
    AbrDecision {
        reason,
        changed: target != current,
        target_variant_index: target,
    }
}

fn sorted_candidates(variants: &[AbrVariant], max_bw: Option<u64>) -> Vec<(usize, u64)> {
    let mut out: Vec<(usize, u64)> = variants
        .iter()
        .filter(|v| max_bw.is_none_or(|cap| v.bandwidth_bps <= cap))
        .map(|v| (v.variant_index, v.bandwidth_bps))
        .collect();
    out.sort_by_key(|(_, bw)| *bw);
    out
}

fn current_bandwidth(sorted: &[(usize, u64)], current: usize) -> u64 {
    sorted
        .iter()
        .find(|(idx, _)| *idx == current)
        .map_or(0, |(_, bw)| *bw)
}

fn adjusted_throughput(estimate_bps: u64, safety_factor: f64) -> f64 {
    let raw = estimate_bps.to_f64().unwrap_or(0.0);
    (raw / safety_factor).max(0.0)
}

fn candidate_variant(sorted: &[(usize, u64)], adjusted_bps: f64) -> Option<(usize, u64)> {
    let best_under = sorted
        .iter()
        .filter(|(_, bw)| bw.to_f64().unwrap_or(f64::INFINITY) <= adjusted_bps)
        .max_by_key(|(_, bw)| *bw);
    best_under
        .or_else(|| sorted.first())
        .map(|(idx, bw)| (*idx, *bw))
}

#[derive(Clone, Copy)]
struct SwitchContext<'a> {
    settings: &'a AbrSettings,
    buffer_ahead: Option<std::time::Duration>,
    adjusted_bps: f64,
    candidate_bw: u64,
    current_bw: u64,
    candidate_idx: usize,
    current: usize,
}

fn up_switch(ctx: SwitchContext<'_>) -> AbrDecision {
    let buffer_ok = ctx
        .buffer_ahead
        .is_none_or(|b| b >= ctx.settings.min_buffer_for_up_switch);
    let candidate_bw_f = ctx.candidate_bw.to_f64().unwrap_or(f64::INFINITY);
    let headroom_ok = ctx.adjusted_bps >= candidate_bw_f * ctx.settings.up_hysteresis_ratio;
    if buffer_ok && headroom_ok {
        return decision(ctx.current, ctx.candidate_idx, AbrReason::UpSwitch);
    }
    decision(ctx.current, ctx.current, AbrReason::BufferTooLowForUpSwitch)
}

fn down_switch(ctx: SwitchContext<'_>) -> Option<AbrDecision> {
    let urgent = ctx
        .buffer_ahead
        .is_some_and(|b| b <= ctx.settings.urgent_downswitch_buffer);
    let current_bw_f = ctx.current_bw.to_f64().unwrap_or(f64::INFINITY);
    let margin_ok = ctx.adjusted_bps <= current_bw_f * ctx.settings.down_hysteresis_ratio;
    if urgent {
        return Some(decision(
            ctx.current,
            ctx.candidate_idx,
            AbrReason::UrgentDownSwitch,
        ));
    }
    if margin_ok {
        return Some(decision(
            ctx.current,
            ctx.candidate_idx,
            AbrReason::DownSwitch,
        ));
    }
    None
}
