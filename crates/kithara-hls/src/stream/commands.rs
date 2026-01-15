//! Stream commands and iteration state.

use tokio::sync::mpsc;

use crate::abr::{AbrController, AbrReason};

/// Commands for stream control.
#[derive(Debug)]
pub enum StreamCommand {
    Seek { segment_index: usize },
    ForceVariant { variant_index: usize, from: usize },
}

/// Segment iteration state.
#[derive(Clone)]
pub struct IterState {
    pub from: usize,
    pub to: usize,
    pub start_segment: usize,
    pub reason: AbrReason,
}

impl IterState {
    pub fn new(variant: usize) -> Self {
        Self {
            from: variant,
            to: variant,
            start_segment: 0,
            reason: AbrReason::Initial,
        }
    }

    pub fn apply_seek(&mut self, segment_index: usize) {
        self.start_segment = segment_index;
    }

    pub fn apply_force_variant(&mut self, variant_index: usize, from: usize) {
        self.from = from;
        self.to = variant_index;
        self.start_segment = 0;
        self.reason = AbrReason::ManualOverride;
    }

    pub fn apply_switch(&mut self, next: &SwitchDecision) {
        self.from = next.from;
        self.to = next.to;
        self.start_segment = next.start_segment;
        self.reason = next.reason;
    }
}

/// Variant switch decision.
pub struct SwitchDecision {
    pub from: usize,
    pub to: usize,
    pub start_segment: usize,
    pub reason: AbrReason,
}

impl SwitchDecision {
    pub fn from_seek(current_to: usize, segment_index: usize) -> Self {
        Self {
            from: current_to,
            to: current_to,
            start_segment: segment_index,
            reason: AbrReason::ManualOverride,
        }
    }

    pub fn from_force(variant_index: usize, from: usize) -> Self {
        Self {
            from,
            to: variant_index,
            start_segment: 0,
            reason: AbrReason::ManualOverride,
        }
    }
}

/// Process pending commands from the command channel.
///
/// If `process_all` is true, processes all pending commands (while loop).
/// If false, processes at most one command (single try_recv).
///
/// Returns `Some(SwitchDecision)` if a command requires a variant/segment switch.
pub fn process_commands(
    cmd_rx: &mut mpsc::Receiver<StreamCommand>,
    state: &mut IterState,
    abr: &mut AbrController,
    process_all: bool,
) -> Option<SwitchDecision> {
    let mut result = None;

    while let Ok(cmd) = cmd_rx.try_recv() {
        match cmd {
            StreamCommand::Seek { segment_index } => {
                if process_all {
                    state.apply_seek(segment_index);
                } else {
                    result = Some(SwitchDecision::from_seek(state.to, segment_index));
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
                    result = Some(SwitchDecision::from_force(variant_index, from));
                }
            }
        }

        if !process_all {
            break;
        }
    }

    result
}
