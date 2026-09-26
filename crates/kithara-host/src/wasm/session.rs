use kithara_bufpool::HasPool;
use kithara_platform::{
    sync::{Arc, Mutex, mpsc},
    thread::{assert_main_thread, assert_not_main_thread},
};
use kithara_warp::BeatGridId;

use crate::{
    Host, PlayError,
    session::{self as host_session, RootView, protocol::HostCmdMsg, web::WebSessionState},
};

fn assert_message_send<S: Send + Sync>() {
    const fn assert_send<T: Send>() {}
    assert_send::<HostCmdMsg<S>>();
}

/// Worker-side endpoint for the canonical Host owned by the main thread.
#[derive_where::derive_where(Clone)]
pub struct HostSender<S> {
    id: BeatGridId,
    root_view: RootView,
    tx: mpsc::Sender<HostCmdMsg<S>>,
}

/// Main-thread receiver for one canonical Host command route.
pub struct HostReceiver<S> {
    route: Arc<HostRoute<S>>,
    state: WebSessionState<S>,
}

pub(crate) struct HostRoute<S> {
    receiver: Mutex<Option<mpsc::Receiver<HostCmdMsg<S>>>>,
}

impl<S> HostRoute<S> {
    fn new(receiver: mpsc::Receiver<HostCmdMsg<S>>) -> Self {
        Self {
            receiver: Mutex::new(Some(receiver)),
        }
    }

    pub(crate) fn close(&self) {
        self.receiver.lock().take();
    }
}

/// Creates the Worker route for an already constructed main-thread Host.
///
/// # Errors
/// Returns an error when `host` is itself a remote Worker facade.
#[must_use]
pub fn worker_host_channel<S: HasPool<f32> + Send + Sync + 'static>(
    host: &Host<S>,
) -> Result<(HostSender<S>, HostReceiver<S>), PlayError> {
    assert_main_thread("worker_host_channel");
    assert_message_send::<S>();
    let (id, root_view) = host.remote_identity();
    let (tx, rx) = host_session::worker_channel();
    let state = host
        .web_state()
        .cloned()
        .ok_or_else(|| PlayError::Internal("worker route requires a local host".into()))?;
    let route = Arc::new(HostRoute::new(rx));
    host.register_remote_route(Arc::clone(&route));
    Ok((
        HostSender { id, root_view, tx },
        HostReceiver { route, state },
    ))
}

/// Connects a Worker facade to the main thread's canonical Host owner.
///
/// # Panics
/// Panics on the main thread, which answers the facade's calls and so cannot
/// wait on them.
#[must_use]
pub fn remote_host<S: HasPool<f32> + Send + Sync + 'static>(sender: HostSender<S>) -> Host<S> {
    assert_not_main_thread("remote_host");
    let dispatcher = host_session::remote(sender.tx, sender.root_view.clone());
    Host::remote(sender.id, sender.root_view, dispatcher)
}

/// Pre-initialise the audio context and AudioWorklet eagerly.
///
/// Call on the main thread after constructing [`Host`]. This creates the
/// AudioContext in suspended state and starts the async AudioWorklet module
/// load. Once complete, `firewheel-web-audio` registers auto-resume listeners
/// so that the very first user click resumes the context.
///
/// # Errors
/// Returns an error for a remote Host or failed audio-context initialisation.
pub fn warm_up_audio<S>(host: &Host<S>) -> Result<(), PlayError>
where
    S: HasPool<f32> + Send + Sync + 'static,
{
    assert_main_thread("warm_up_audio");
    let state = host
        .web_state()
        .ok_or_else(|| PlayError::Internal("audio warm-up requires a local host".into()))?;
    host_session::warm_up_audio(state).map_err(PlayError::from)
}

/// Poll pending session commands from Workers and update the audio graph.
///
/// Call this on the main thread from `requestAnimationFrame`.
pub fn tick_and_poll<S>(receiver: &HostReceiver<S>)
where
    S: HasPool<f32> + Send + Sync + 'static,
{
    assert_main_thread("tick_and_poll");
    let route = receiver.route.receiver.lock();
    if let Some(rx) = route.as_ref() {
        host_session::tick_and_poll_remote(&receiver.state, rx);
    }
}
