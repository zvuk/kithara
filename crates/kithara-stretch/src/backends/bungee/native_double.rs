//! `bungee_sys::stretcher` for tests: every call reaches the native library,
//! and an armed fault corrupts the next result of one call.

use std::cell::Cell;

pub(super) use bungee_sys::stretcher::{
    analyse_grain, create, destroy, is_flushed, max_input_frame_count, next, preroll,
};
use bungee_sys::{BungeeStretcher, InputChunk, OutputChunk, Request};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum NativeFault {
    Analyse,
    Synthesise,
}

thread_local! {
    static ARMED: Cell<Option<NativeFault>> = const { Cell::new(None) };
}

pub(super) fn fail_next(fault: NativeFault) {
    ARMED.set(Some(fault));
}

fn take(fault: NativeFault) -> bool {
    let armed = ARMED.get() == Some(fault);
    if armed {
        ARMED.set(None);
    }
    armed
}

/// An analysis fault requests more input than one grain can hold.
pub(super) fn specify_grain(
    stretcher: *mut BungeeStretcher,
    request: &Request,
    buffer_start_position: f64,
) -> InputChunk {
    let chunk = bungee_sys::stretcher::specify_grain(stretcher, request, buffer_start_position);
    if take(NativeFault::Analyse) {
        return InputChunk {
            begin: chunk.begin,
            end: chunk.begin.saturating_add(i32::MAX),
        };
    }
    chunk
}

/// A synthesis fault reports a negative output frame count.
pub(super) fn synthesise_grain(stretcher: *mut BungeeStretcher, output_chunk: &mut OutputChunk) {
    bungee_sys::stretcher::synthesise_grain(stretcher, &mut *output_chunk);
    if take(NativeFault::Synthesise) {
        output_chunk.frame_count = -1;
    }
}
