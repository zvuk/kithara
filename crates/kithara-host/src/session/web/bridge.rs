use std::{cell::RefCell, num::NonZeroU32, sync::atomic::Ordering};

use firewheel::FirewheelCtx;
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
            .map_or(0.0, |s| s.position.load(Ordering::Relaxed))
    })
}

pub(crate) fn bridge_duration_secs() -> f64 {
    BRIDGE_PLAYBACK.with(|cell| {
        cell.borrow()
            .as_ref()
            .map_or(0.0, |s| s.duration.load(Ordering::Relaxed))
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
    ctx: &mut FirewheelCtx<firewheel_web_audio::WebAudioBackend>,
    sample_rate: u32,
) -> Result<(), String> {
    let config = firewheel_web_audio::WebAudioConfig {
        sample_rate: NonZeroU32::new(sample_rate),
        request_input: false,
    };
    ctx.start_stream(config).map_err(|err| err.to_string())
}
