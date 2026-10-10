use std::{marker::PhantomData, ops::Deref};

use kithara_bufpool::HasPool;
use kithara_command::{Live, Seq, When};
use kithara_config::{ConfigOwner, Configure};
use kithara_output::OutputGroup;
use kithara_platform::{maybe_send::MaybeSend, sync::Arc};
use kithara_play::{HostedDeck, PlayError};
use kithara_signal::SessionFrame;
use kithara_warp::{BeatGrid, BeatGridId};

#[cfg(feature = "offline")]
use super::offline::OfflineRuntime;
use super::{
    HostConfig, HostSettings, HostSettingsChange,
    platform::{Platform, PlatformResult},
};
use crate::{
    DeckControl, DeckId, HostCommand, HostCore, HostOwner,
    api::Tap,
    session::{HostDispatcher, HostRoot, RootView, SessionError, SessionSampleRate, ask},
};

/// The control endpoint of a deck held exclusively by a Host owner.
#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub struct HostOwned<D: DeckControl> {
    host_id: BeatGridId,
    #[field(get, copy)]
    id: DeckId,
    #[field(get)]
    control: D::Control,
    marker: PhantomData<fn() -> D>,
}

impl<D: DeckControl> Deref for HostOwned<D> {
    type Target = D::Control;
    fn deref(&self) -> &Self::Target {
        &self.control
    }
}

/// A handle whose session drives one canonical owner, optionally decorated.
pub struct Host<S, O: HostOwner<S> = HostCore<S>> {
    pub(in crate::host) dispatcher: Arc<dyn HostDispatcher<O::Command>>,
    pub(in crate::host) id: BeatGridId,
    pub(in crate::host) root_view: RootView,
    pub(in crate::host) _session: SessionRuntime<S, O>,
    pub(in crate::host) owns_session: bool,
}

pub(in crate::host) enum SessionRuntime<S, O: HostOwner<S>> {
    Realtime {
        _platform: Platform<S, O>,
    },
    #[cfg(feature = "offline")]
    Offline {
        platform: Platform<S, O>,
        runtime: OfflineRuntime<S, O>,
    },
}

#[cfg(feature = "offline")]
type OfflineSessionMut<'a, S, O> = (&'a Platform<S, O>, &'a mut OfflineRuntime<S, O>);

impl<S, O: HostOwner<S>> SessionRuntime<S, O> {
    #[cfg(feature = "offline")]
    pub(in crate::host) const fn offline_mut(&mut self) -> Option<OfflineSessionMut<'_, S, O>> {
        match self {
            Self::Offline { platform, runtime } => Some((platform, runtime)),
            Self::Realtime { .. } => None,
        }
    }
    #[cfg(target_arch = "wasm32")]
    pub(in crate::host) const fn platform(&self) -> &Platform<S, O> {
        match self {
            Self::Realtime { _platform } => _platform,
            #[cfg(feature = "offline")]
            Self::Offline { platform, .. } => platform,
        }
    }
}

pub(in crate::host) struct SessionRoot {
    pub(in crate::host) id: BeatGridId,
    pub(in crate::host) root: HostRoot,
    pub(in crate::host) view: RootView,
}

impl<S, O: HostOwner<S>> Host<S, O> {
    delegate::delegate! {
        to self.root_view {
            /// Whether the session holds no deck.
            #[must_use]
            pub fn is_empty(&self) -> bool;
            /// The measured output rate and the settings' requested rate.
            #[must_use]
            #[call(sample_rate)]
            pub fn output_sample_rate(&self) -> SessionSampleRate;
        }
    }

    /// Posts an owner command and returns its ticket number without waiting.
    /// The owner answers after checks and sending; effects publish on receipts.
    ///
    /// # Errors
    /// Returns [`PlayError::SessionGone`] if the owner no longer accepts posts.
    pub fn send(&self, command: O::Command) -> Result<Seq, PlayError> {
        self.dispatcher
            .dispatch(command)
            .map(|ticket| ticket.seq())
            .map_err(Into::into)
    }

    /// Allocates a fresh identity for a deck registered through this handle.
    ///
    /// # Errors
    /// Returns [`PlayError::Session`] with [`SessionError::BeatGridIdAllocation`]
    /// when the identity space is exhausted.
    pub fn deck_id(&self) -> Result<DeckId, PlayError> {
        DeckId::allocate().map_err(|error| SessionError::from(error).into())
    }

    fn ask(&self, command: HostCommand<S, O::Deck>) -> Result<(), PlayError> {
        ask(&*self.dispatcher, command.into()).map_err(Into::into)
    }

    /// Attaches one output group to a session tap.
    ///
    /// # Errors
    /// Returns [`PlayError::Session`] with [`SessionError::TapActive`] when the
    /// tap already has a consumer, or [`SessionError::Graph`] on graph failure.
    /// Returns [`PlayError::SessionGone`] if the owner stops or drops the post.
    pub fn attach_outputs(&self, tap: Tap, outputs: OutputGroup) -> Result<(), PlayError> {
        self.ask(HostCommand::AttachOutputs { tap, outputs })
    }

    /// Detaches the output group of a session tap.
    ///
    /// # Errors
    /// Returns [`PlayError::SessionGone`] if the owner stops or drops the post.
    pub fn detach_outputs(&self, tap: Tap) -> Result<(), PlayError> {
        self.ask(HostCommand::DetachOutputs { tap })
    }

    /// Restarts the owner's route while retaining its held decks.
    ///
    /// # Errors
    /// Returns [`PlayError::Session`] with [`SessionError::RestartFailed`] if
    /// the backend or graph fails to restart. Returns [`PlayError::SessionGone`]
    /// if the owner stops or drops the post.
    pub fn invalidate_audio_route<R: Into<String>>(&self, reason: R) -> Result<(), PlayError> {
        tracing::debug!(reason = %reason.into(), "host route restart requested");
        self.ask(HostCommand::Restart)
    }

    pub(in crate::host) fn session_root(settings: HostSettings) -> Result<SessionRoot, PlayError> {
        let id = BeatGridId::allocate().map_err(SessionError::from)?;
        let root = HostRoot::new(id, settings.sample_rate());
        let view = RootView::new(&root, settings);
        Ok(SessionRoot { id, root, view })
    }

    pub(in crate::host) fn owner(
        id: BeatGridId,
        root_view: RootView,
        dispatcher: Arc<dyn HostDispatcher<O::Command>>,
        session: SessionRuntime<S, O>,
    ) -> Self {
        Self {
            id,
            root_view,
            dispatcher,
            _session: session,
            owns_session: true,
        }
    }

    fn validate_removal<D: DeckControl>(&self, deck: &HostOwned<D>) -> Result<(), PlayError> {
        if deck.host_id != self.id {
            return Err(PlayError::ForeignSession);
        }
        if !self.root_view.holds(deck.id()) {
            return Err(SessionError::DeckNotFound(deck.id()).into());
        }
        Ok(())
    }

    /// Closes and releases a deck on the canonical owner thread.
    ///
    /// Removing the last live deck stops the output and settles without a render.
    /// An offline owner also settles non-last decks between processor turns,
    /// without requiring another render; surviving decks remain available.
    ///
    /// # Errors
    /// Returns [`PlayError::ForeignSession`] for another host's deck, or
    /// [`PlayError::Session`] with [`SessionError::DeckNotFound`] for a missing
    /// deck. Propagates the deck's close refusal, [`PlayError::Closed`] for a
    /// retired channel, [`PlayError::Internal`] for scope retirement failure,
    /// and session graph errors when stopping the final deck. Returns
    /// [`PlayError::SessionGone`] if the owner stops or drops the post.
    pub fn remove<D: DeckControl>(&mut self, deck: &HostOwned<D>) -> Result<(), PlayError> {
        self.validate_removal(deck)?;
        self.ask(HostCommand::Release(deck.id()))
    }
}

impl<S, O> Host<S, O>
where
    S: HasPool<f32> + Send + Sync + 'static,
    O: HostOwner<S>,
{
    /// Creates a Host whose session drives the decorated base owner.
    ///
    /// # Errors
    /// Returns [`PlayError::InvalidParameter`] for invalid settings and
    /// [`PlayError::Session`] with [`SessionError::BeatGridIdAllocation`] when
    /// identities are exhausted. Browser graph or stream startup failures also
    /// return [`PlayError::Session`]; offline task reservation or startup failures
    /// return [`PlayError::Internal`].
    pub fn layered<L>(config: HostConfig<S>, layer: L) -> Result<Self, PlayError>
    where
        L: FnOnce(HostCore<S, O::Deck>) -> O + MaybeSend + 'static,
    {
        let channel_config = config.channel_config();
        match config {
            HostConfig::Realtime {
                output_block_frames,
                limiter,
                settings,
                ..
            } => {
                let settings = Live::new(settings)?;
                let root = Self::session_root(*settings.config())?;
                let (dispatcher, platform) = Platform::realtime(
                    root.root,
                    root.view.clone(),
                    output_block_frames,
                    channel_config,
                    crate::rt::SessionOutput::new(limiter),
                    settings,
                    layer,
                )
                .resolve()?;
                Ok(Self::owner(
                    root.id,
                    root.view,
                    dispatcher,
                    SessionRuntime::Realtime {
                        _platform: platform,
                    },
                ))
            }
            #[cfg(feature = "offline")]
            config @ HostConfig::Offline { .. } => {
                let root = Self::session_root(config.settings())?;
                let (dispatcher, platform, runtime) =
                    Platform::offline(config, root.root, root.view.clone(), layer)?;
                Ok(Self::owner(
                    root.id,
                    root.view,
                    dispatcher,
                    SessionRuntime::Offline { platform, runtime },
                ))
            }
        }
    }
}

impl<S> Host<S>
where
    S: HasPool<f32> + Send + Sync + 'static,
{
    /// Creates a realtime or offline Host with its base owner.
    ///
    /// # Errors
    /// Returns [`PlayError::InvalidParameter`] for invalid settings,
    /// [`PlayError::Session`] for exhausted identities or browser startup
    /// failures, and [`PlayError::Internal`] for offline task reservation or
    /// startup failures.
    pub fn new(config: HostConfig<S>) -> Result<Self, PlayError> {
        Self::layered(config, |core| core)
    }

    /// Transfers a fully constructed deck, retaining only its public control handle.
    ///
    /// # Errors
    /// Returns [`PlayError::Internal`] when deck identities are exhausted.
    /// Propagates the registration errors described by [`HostOwner::register`],
    /// or [`PlayError::SessionGone`] if the owner stops or drops the post.
    pub fn insert<D>(&mut self, deck: D) -> Result<HostOwned<D>, PlayError>
    where
        D: HostedDeck<S> + DeckControl,
    {
        let id = DeckId::allocate().map_err(|error| PlayError::Internal(error.to_string()))?;
        let control = deck.control();
        self.ask(HostCommand::Register {
            id,
            deck: Box::new(deck),
        })?;
        Ok(HostOwned {
            host_id: self.id,
            id,
            control,
            marker: PhantomData,
        })
    }
}

impl<S, O: HostOwner<S>> Configure<HostSettingsChange> for Host<S, O> {
    type At = When<SessionFrame>;
    type Config = HostSettings;
    type Error = PlayError;
    type Output = ();
    /// Answers after the owner accepts and sends the change.
    /// Settings remain confirmed state, published on the executor receipt.
    fn configure(&self, change: HostSettingsChange, at: Self::At) -> Result<(), PlayError> {
        self.ask(HostCommand::Configure(change, at))
    }
    fn settings(&self) -> HostSettings {
        self.root_view.settings()
    }
}

impl<S, O: HostOwner<S>> Drop for Host<S, O> {
    fn drop(&mut self) {
        if self.owns_session {
            self.dispatcher.shutdown();
        }
    }
}

impl<S: Send + Sync + 'static, O: HostOwner<S>> BeatGrid for Host<S, O> {
    fn id(&self) -> BeatGridId {
        self.id
    }
    fn snapshot(&self) -> kithara_warp::BeatGridSnapshot {
        self.root_view.grid()
    }
}
