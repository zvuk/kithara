use std::sync::OnceLock;

use kithara::{
    host::{HostConfig, HostOwned, HostSettingsControl},
    platform::sync::Mutex,
    play::{DeckControl, HostedDeck, PlayError},
};

use crate::{
    player::AudioPlayer,
    pools::{FfiHost, FfiPools},
    types::{FfiDuckingMode, FfiError},
};

static HOST: OnceLock<Mutex<Option<FfiHost>>> = OnceLock::new();

fn host() -> &'static Mutex<Option<FfiHost>> {
    HOST.get_or_init(|| Mutex::new(None))
}

fn active_host(slot: &mut Option<FfiHost>) -> Result<&mut FfiHost, PlayError> {
    match slot {
        Some(host) => Ok(host),
        None => Ok(slot.insert(FfiHost::new(HostConfig::builder().build())?)),
    }
}

pub(crate) fn insert<P>(player: P) -> Result<HostOwned<P>, PlayError>
where
    P: HostedDeck<FfiPools> + DeckControl,
{
    active_host(&mut host().lock())?.insert(player)
}

fn with_active<R>(apply: impl FnOnce(&FfiHost) -> Result<R, PlayError>) -> Result<R, PlayError> {
    host().lock().as_ref().map_or(
        Err(PlayError::SessionGone {
            reason: "process audio Host is unavailable",
        }),
        apply,
    )
}

pub(crate) fn remove<P>(player: &HostOwned<P>) -> Result<(), PlayError>
where
    P: DeckControl,
{
    let mut slot = host().lock();
    let active = slot.as_mut().ok_or(PlayError::SessionGone {
        reason: "process audio Host is unavailable",
    })?;
    active.remove(player)?;
    if active.is_empty() {
        drop(slot.take());
    }
    drop(slot);
    Ok(())
}

/// Platform audio-session signals the process Host answers: route changes
/// and competing sounds.
#[cfg_attr(feature = "uniffi", uniffi::export)]
impl AudioPlayer {
    /// Notify the native player that the platform audio route changed.
    ///
    /// This does not change queue state. If playback is active, the
    /// native output stream is recreated so CoreAudio/CPAL cannot keep a
    /// stale route after headphones or Bluetooth devices are removed.
    ///
    /// # Errors
    ///
    /// Returns [`FfiError`] when the native player cannot schedule the
    /// route invalidation.
    pub fn notify_audio_route_changed(&self, reason: &str) -> Result<(), FfiError> {
        with_active(|host| host.invalidate_audio_route(reason)).map_err(FfiError::from)
    }

    /// Lower or restore the whole session output under a competing sound,
    /// such as a call or a navigation prompt.
    ///
    /// # Errors
    ///
    /// Returns [`FfiError`] when the audio session rejects the change.
    pub fn set_ducking_mode(&self, mode: FfiDuckingMode) -> Result<(), FfiError> {
        with_active(|host| host.set_ducking(mode.into())).map_err(FfiError::from)
    }
}
