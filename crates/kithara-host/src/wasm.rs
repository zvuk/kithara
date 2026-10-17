use std::task::{Wake, Waker};

use kithara_bufpool::HasPool;
use kithara_platform::{
    sync::{Arc, Mutex, Notify},
    thread::{assert_main_thread, assert_not_main_thread},
};
use kithara_play::HostedDeck;

use crate::{
    Host, HostCommand, HostCore, HostOwner, PlayError,
    session::{
        RootView,
        decks::{DeckInbox, DeckMsg},
        dispatch::OwnerPosts,
        protocol::{HostMailbox, HostPostbox},
        web::WebSessionState,
    },
};

/// A Worker endpoint to the canonical owner's existing command queue.
#[derive_where::derive_where(Clone)]
pub struct HostSender<S> {
    pub(crate) id: crate::DeckId,
    pub(crate) root_view: RootView,
    pub(crate) postbox: HostPostbox<HostCommand<S, dyn HostedDeck<S>>>,
}

/// The main-thread endpoint driving the same canonical owner.
pub struct HostReceiver<S: 'static> {
    pub(crate) state: WebSessionState<HostCore<S>>,
    pub(crate) route: Arc<HostRoute<HostCommand<S, dyn HostedDeck<S>>>>,
}

pub(crate) struct HostRoute<C> {
    pub(crate) postbox: HostPostbox<C>,
    mailbox: Mutex<Option<HostMailbox<C>>>,
    posts: Mutex<OwnerPosts>,
    pub(crate) wake: Arc<HostWake>,
}

pub(crate) struct HostWake {
    notify: Arc<Notify>,
    decks: Mutex<Vec<DeckMsg>>,
}

struct DeckWake {
    host: Arc<HostWake>,
    message: DeckMsg,
}

impl Wake for DeckWake {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        drop(self.host.post(self.message));
    }
}

impl HostWake {
    pub(crate) fn notified(&self) -> impl std::future::Future<Output = ()> {
        self.notify.notified()
    }
}

impl Wake for HostWake {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.notify.notify_one();
    }
}

impl DeckInbox for HostWake {
    fn post(&self, message: DeckMsg) -> Result<(), PlayError> {
        self.decks.lock().push(message);
        self.notify.notify_one();
        Ok(())
    }

    fn waker(self: Arc<Self>, message: DeckMsg) -> Waker {
        Waker::from(Arc::new(DeckWake {
            host: self,
            message,
        }))
    }
}

impl<C> HostRoute<C> {
    pub(crate) fn new(postbox: HostPostbox<C>, mut mailbox: HostMailbox<C>) -> Self {
        let wake = Arc::new(HostWake {
            notify: Arc::new(Notify::new()),
            decks: Mutex::new(Vec::new()),
        });
        mailbox.hold(Waker::from(wake.clone()));
        Self {
            postbox,
            mailbox: Mutex::new(Some(mailbox)),
            posts: Mutex::new(OwnerPosts::new()),
            wake,
        }
    }

    pub(crate) fn close(&self) {
        self.mailbox.lock().take();
        *self.posts.lock() = OwnerPosts::new();
        self.wake.decks.lock().clear();
        self.wake.notify.notify_one();
    }

    pub(crate) fn drain<S, O: HostOwner<S, Command = C>>(
        &self,
        state: &WebSessionState<O>,
    ) -> bool {
        assert_main_thread("host web pass");
        let mut state = state.lock();
        let Some(owner) = state.as_mut() else {
            return false;
        };
        owner.begin_pass();
        let mut posts = self.posts.lock();
        if let Some(mailbox) = self.mailbox.lock().as_mut() {
            posts.drain(owner, mailbox);
        }
        let messages = std::mem::take(&mut *self.wake.decks.lock());
        for message in messages {
            message.run(owner);
        }
        posts.pass(owner, true);
        true
    }
}

/// Shares the existing owner's route with a Worker without transferring decks twice.
pub fn worker_host_channel<S: HasPool<f32> + Send + Sync + 'static>(
    host: &Host<S>,
) -> Result<(HostSender<S>, HostReceiver<S>), PlayError> {
    assert_main_thread("worker_host_channel");
    host.browser_channel()
}

/// Connects a Worker facade to the existing main-thread owner.
pub fn remote_host<S: HasPool<f32> + Send + Sync + 'static>(sender: HostSender<S>) -> Host<S> {
    assert_not_main_thread("remote_host");
    Host::browser_remote(sender)
}

/// Runs pending owner commands and one owner pass on the main thread.
pub fn tick_and_poll<S: HasPool<f32> + Send + Sync + 'static>(receiver: &HostReceiver<S>) {
    assert_main_thread("tick_and_poll");
    receiver.route.drain::<S, HostCore<S>>(&receiver.state);
}
