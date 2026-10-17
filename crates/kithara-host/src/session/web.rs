use std::{marker::PhantomData, num::NonZeroU32};

use firewheel::FirewheelContext;
use firewheel_web_audio::WebAudioBackend;
use kithara_bufpool::HasPool;
use kithara_command::{Live, ScopedConfig, Ticket, mailbox};
use kithara_platform::{
    maybe_send::MaybeSend,
    sync::{Arc, Mutex},
    thread::assert_main_thread,
    time::sleep,
    tokio::{select, task::spawn as spawn_task},
};

use crate::{
    HostCore, HostOwner, HostSettings, PlayError,
    consts::SESSION_PUMP_INTERVAL,
    rt::SessionOutput,
    session::{
        decks::DeckInbox,
        protocol::{HostDispatchError, HostDispatcher, HostPostbox, not_taken},
        queue::HostProtocol,
        state::{HostRoot, RootView, SessionBufferConfig, SessionState, SessionStream, ensure_ctx},
    },
    wasm::HostRoute,
};

pub(crate) type WebSessionState<O> = Arc<Mutex<Option<O>>>;

enum SessionHost<C, O> {
    Local {
        state: WebSessionState<O>,
        route: Arc<HostRoute<C>>,
    },
    Remote,
}

pub(crate) struct SessionClient<S, O: HostOwner<S>> {
    postbox: HostPostbox<O::Command>,
    host: SessionHost<O::Command, O>,
    marker: PhantomData<fn() -> S>,
}

impl<S, O: HostOwner<S>> HostDispatcher<O::Command> for SessionClient<S, O> {
    fn dispatch(&self, command: O::Command) -> Result<Ticket<PlayError>, HostDispatchError> {
        let ticket = self.postbox.post(command).map_err(not_taken)?;
        if let SessionHost::Local { state, route } = &self.host {
            route.drain::<S, O>(state);
        }
        Ok(ticket)
    }
    fn shutdown(&self) {
        if let SessionHost::Local { state, route } = &self.host {
            route.close();
            state.lock().take();
        }
    }
}

pub(crate) fn spawn<S, O>(
    root: HostRoot,
    view: RootView,
    channel_config: ScopedConfig,
    output: SessionOutput,
    settings: Live<HostSettings, HostProtocol>,
    layer: impl FnOnce(HostCore<S, O::Deck>) -> O + MaybeSend + 'static,
) -> Result<
    (
        Arc<dyn HostDispatcher<O::Command>>,
        WebSessionState<O>,
        Arc<HostRoute<O::Command>>,
    ),
    PlayError,
>
where
    S: HasPool<f32> + Send + Sync + 'static,
    O: HostOwner<S>,
{
    assert_main_thread("host web spawn");
    let (postbox, mailbox) = mailbox();
    let route = Arc::new(HostRoute::new(postbox.clone(), mailbox));
    let mut session = SessionState::new(
        root,
        view,
        SessionBufferConfig::default(),
        output,
        settings,
        channel_config,
        |ctx, rate| {
            start_stream_web_audio(ctx, rate)
                .map(|backend| SessionStream::Realtime { _backend: backend })
        },
    );
    session.delivery_delay = Some(SESSION_PUMP_INTERVAL);
    // A browser unlocks its output through a user gesture and can never resume
    // a closed `AudioContext`: releasing the device on idle is irreversible, so
    // every later context stays suspended and the render callback never runs
    // again. This session holds its device for as long as it lives.
    session.retains_output = true;
    ensure_ctx(&mut session)?;
    let inbox: Arc<dyn DeckInbox> = route.wake.clone();
    let state = Arc::new(Mutex::new(Some(layer(HostCore::new(session, inbox)))));
    let dispatcher = Arc::new(SessionClient {
        postbox,
        host: SessionHost::Local {
            state: state.clone(),
            route: route.clone(),
        },
        marker: PhantomData,
    });
    let pump_state = state.clone();
    let pump_route = route.clone();
    drop(spawn_task(async move {
        loop {
            select! {
                _ = pump_route.wake.notified() => {},
                _ = sleep(SESSION_PUMP_INTERVAL) => {},
            }
            if !pump_route.drain::<S, O>(&pump_state) {
                break;
            }
        }
    }));
    Ok((dispatcher, state, route))
}

pub(crate) fn remote<S: 'static, O: HostOwner<S>>(
    postbox: HostPostbox<O::Command>,
) -> Arc<dyn HostDispatcher<O::Command>> {
    Arc::new(SessionClient::<S, O> {
        postbox,
        host: SessionHost::Remote,
        marker: PhantomData,
    })
}

fn start_stream_web_audio(
    ctx: &mut FirewheelContext,
    sample_rate: u32,
) -> Result<WebAudioBackend, String> {
    WebAudioBackend::new(
        ctx,
        firewheel_web_audio::WebAudioConfig {
            sample_rate: NonZeroU32::new(sample_rate),
            request_input: false,
        },
    )
    .map_err(|error| error.to_string())
}
