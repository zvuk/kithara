use std::num::NonZeroU32;

use firewheel::FirewheelContext;
use kithara_bufpool::HasPool;
use kithara_command::{Live, ScopeId, ScopedReceipt, When};
use kithara_effects::LimiterConfig;
use kithara_events::EventBus;
use kithara_play::{PlayError, SessionTransportSnapshot};
use kithara_render::{bridge::scope_channels, rt::DeckMixerConfig};
use kithara_test_utils::bufpool::{TestPools, pools};
use kithara_warp::BeatGridId;

use super::super::{
    dispatch::tick_session,
    graph::{drop_idle_context, idle, install_deck, remove_deck},
    protocol::SessionError,
    queue::settle_receipt,
    state::{HostRoot, RootView, SessionState},
    transport::observe_commits,
};
use crate::{HostSettings, HostSettingsChange, HostSettingsExec, rt::SessionOutput};

pub(crate) struct GraphSession<T> {
    state: SessionState<T, TestPools>,
    scopes: Vec<(BeatGridId, ScopeId)>,
}

impl<T> std::ops::Deref for GraphSession<T> {
    type Target = SessionState<T, TestPools>;
    fn deref(&self) -> &Self::Target {
        &self.state
    }
}

impl<T> std::ops::DerefMut for GraphSession<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.state
    }
}

impl<T> GraphSession<T> {
    pub(crate) const DEFAULT_SAMPLE_RATE: NonZeroU32 = match NonZeroU32::new(44_100) {
        Some(rate) => rate,
        None => unreachable!(),
    };

    pub(crate) fn new<F>(start: F) -> Self
    where
        F: FnMut(&mut FirewheelContext, u32) -> Result<T, String> + Send + 'static,
    {
        Self::with_sample_rate(Self::DEFAULT_SAMPLE_RATE, start)
    }

    pub(crate) fn with_sample_rate<F>(rate: NonZeroU32, start: F) -> Self
    where
        F: FnMut(&mut FirewheelContext, u32) -> Result<T, String> + Send + 'static,
    {
        Self {
            state: state_for(rate, start),
            scopes: Vec::new(),
        }
    }

    pub(crate) fn ctx_mut(&mut self) -> Option<&mut FirewheelContext> {
        self.state.ctx.as_mut()
    }

    pub(crate) fn tick(&mut self) -> Result<(), SessionError> {
        self.state.begin_iteration();
        tick_session(&mut self.state)?;
        self.settle();
        Ok(())
    }

    pub(crate) fn install(&mut self, id: BeatGridId, bus: EventBus) -> Result<(), PlayError> {
        super::super::state::ensure_ctx(&mut self.state)?;
        let config = DeckMixerConfig::default();
        let scope = self
            .state
            .channel
            .as_mut()
            .expect("graph channel")
            .open(config.slots().get())
            .expect("fixture scope");
        let (_ends, inputs) = scope_channels(scope, config);
        install_deck(&mut self.state, id, inputs, pools(), Some(bus))?;
        self.scopes.push((id, scope));
        Ok(())
    }

    pub(crate) fn remove(&mut self, id: BeatGridId) -> Result<(), PlayError> {
        let index = self
            .scopes
            .iter()
            .position(|(held, _)| *held == id)
            .expect("installed graph scope");
        let (_, scope) = self.scopes.remove(index);
        self.state
            .channel
            .as_mut()
            .expect("graph channel")
            .close(scope)
            .expect("live graph scope");
        self.state
            .channel
            .as_mut()
            .expect("graph channel")
            .publish()
            .expect("publish scope close");
        remove_deck(&mut self.state, id)?;
        if self.scopes.is_empty() {
            idle(&mut self.state)?;
            self.settle();
            drop_idle_context(&mut self.state)?;
        }
        Ok(())
    }

    pub(crate) fn configure(
        &mut self,
        change: HostSettingsChange,
        at: When<kithara_signal::SessionFrame>,
    ) -> Result<(), PlayError> {
        self.state.begin_iteration();
        self.state.exec(change, at, &mut ())?;
        self.state
            .channel
            .as_mut()
            .expect("graph channel")
            .publish()
            .map_err(|_| PlayError::Closed)
    }

    fn settle(&mut self) {
        while let Some(receipt) = self
            .state
            .channel
            .as_mut()
            .and_then(kithara_command::ScopedSender::receipt)
        {
            if let ScopedReceipt::Root(receipt) = receipt {
                settle_receipt(&mut self.state, &receipt);
            }
        }
    }

    pub(crate) fn stream_mut(&mut self) -> Option<&mut T> {
        self.state.stream.as_mut()
    }

    pub(crate) fn transport(&mut self) -> Option<SessionTransportSnapshot> {
        self.settle();
        committed_transport(&mut self.state)
    }

    pub(crate) fn view(&self) -> RootView {
        self.state.root_view.clone()
    }
}

pub(crate) fn state<T, F>(start: F) -> GraphSession<T>
where
    F: FnMut(&mut FirewheelContext, u32) -> Result<T, String> + Send + 'static,
{
    GraphSession::new(start)
}

pub(crate) fn state_for<T, F, S>(sample_rate: NonZeroU32, start_stream_fn: F) -> SessionState<T, S>
where
    F: FnMut(&mut FirewheelContext, u32) -> Result<T, String> + Send + 'static,
{
    let (root, root_view) = empty_root(sample_rate);
    let settings = HostSettings::builder().sample_rate(sample_rate).build();
    SessionState::new(
        root,
        root_view,
        super::super::state::SessionBufferConfig::default(),
        SessionOutput::new(LimiterConfig::default()),
        Live::new(settings).expect("fixture settings"),
        crate::HostConfig::<S>::builder()
            .settings(settings)
            .build()
            .channel_config(),
        start_stream_fn,
    )
}

pub(crate) fn empty_root(sample_rate: NonZeroU32) -> (HostRoot, RootView) {
    let root = HostRoot::new(
        BeatGridId::allocate().expect("fixture grid id"),
        sample_rate,
    );
    let view = RootView::new(
        &root,
        HostSettings::builder().sample_rate(sample_rate).build(),
    );
    (root, view)
}

pub(crate) fn committed_transport<T, S: HasPool<f32>>(
    state: &mut SessionState<T, S>,
) -> Option<SessionTransportSnapshot> {
    observe_commits(state);
    if state.reserved_session_grid.is_some() {
        return None;
    }
    state.transport_observation.as_mut()?.read().snapshot()
}
