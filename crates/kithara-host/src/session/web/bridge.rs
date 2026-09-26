use std::{cell::RefCell, num::NonZeroU32, sync::atomic::Ordering};

use firewheel::FirewheelContext;
use firewheel_web_audio::WebAudioBackend;
use kithara_bufpool::HasPool;
use kithara_platform::sync::{Arc, mpsc};

use super::client::WebSessionState;
use crate::{
    bridge::PlaybackShared,
    session::{
        dispatch::drain_host_channel,
        protocol::{HostCmdMsg, HostReply, Reply},
        state::ensure_ctx,
    },
};

thread_local! {
    static BRIDGE_PLAYBACK: RefCell<Option<Arc<PlaybackShared>>> = const { RefCell::new(None) };
}

pub(super) fn init_bridge_state() {
    reset_bridge_state();
}

pub(super) fn reset_bridge_state() {
    BRIDGE_PLAYBACK.with(|playback| {
        playback.borrow_mut().take();
    });
}

/// The one tick point that feeds the web stream's clock timestamps and notices a terminated
/// worklet, since Firewheel no longer owns the backend and nothing else polls the stream on the
/// session's behalf.
pub(crate) fn tick_and_poll_remote<S>(
    state: &WebSessionState<S>,
    rx: &mpsc::Receiver<HostCmdMsg<S>>,
) where
    S: HasPool<f32> + Send + Sync + 'static,
{
    let mut state = state.lock();
    let Some(state) = state.as_mut() else {
        return;
    };

    if let Some(stream) = state.stream.as_mut()
        && stream.poll().is_err()
    {
        state.stream = None;
    }

    drain_host_channel(state, rx, |reply| {
        if let HostReply::Play(Reply::SlotAllocated(allocated)) = reply {
            BRIDGE_PLAYBACK.with(|playback| {
                *playback.borrow_mut() = Some(Arc::clone(&allocated.control.playback));
            });
        }
    });
}

pub(crate) fn bridge_position_secs() -> f64 {
    BRIDGE_PLAYBACK.with(|cell| {
        cell.borrow()
            .as_ref()
            .map_or(0.0, |s| s.snapshot().position())
    })
}

pub(crate) fn bridge_duration_secs() -> f64 {
    BRIDGE_PLAYBACK.with(|cell| {
        cell.borrow()
            .as_ref()
            .map_or(0.0, |s| s.snapshot().duration())
    })
}

/// Number of audio-thread process calls the slot has served.
///
/// Monotonic, so a reader samples twice and looks at the delta. In a browser
/// the render callback runs inside an `AudioWorkletProcessor` the page cannot
/// see: when the browser stops calling it — Firefox terminates a `process`
/// that overruns its watchdog — the session still reports itself as playing
/// and the position simply stops. A delta of zero separates that from a
/// callback that runs and finds nothing to play.
pub(crate) fn bridge_process_calls() -> u64 {
    BRIDGE_PLAYBACK.with(|cell| {
        cell.borrow()
            .as_ref()
            .map_or(0, |s| s.process_count.load(Ordering::Relaxed))
    })
}

/// Number of underruns the audio thread has recorded. Read alongside
/// [`bridge_process_calls`]: a callback that runs while this climbs is
/// starving, not stopped.
pub(crate) fn bridge_underruns() -> u64 {
    BRIDGE_PLAYBACK.with(|cell| {
        cell.borrow()
            .as_ref()
            .map_or(0, |s| s.metrics().snapshot().underruns())
    })
}

pub(crate) fn bridge_is_playing() -> bool {
    BRIDGE_PLAYBACK.with(|cell| {
        cell.borrow()
            .as_ref()
            .is_some_and(|s| s.playing.load(Ordering::Relaxed))
    })
}

pub(crate) fn warm_up_audio<S>(
    state: &WebSessionState<S>,
) -> Result<(), crate::session::SessionError> {
    let mut state = state.lock();
    let Some(state) = state.as_mut() else {
        return Err(crate::session::SessionError::Graph(
            "local web session state is unavailable".to_owned(),
        ));
    };
    ensure_ctx(state, state.sample_rate_hint)
}

pub(super) fn start_stream_web_audio(
    ctx: &mut FirewheelContext,
    sample_rate: u32,
) -> Result<WebAudioBackend, String> {
    let config = firewheel_web_audio::WebAudioConfig {
        sample_rate: NonZeroU32::new(sample_rate),
        request_input: false,
    };
    WebAudioBackend::new(ctx, config).map_err(|err| err.to_string())
}
