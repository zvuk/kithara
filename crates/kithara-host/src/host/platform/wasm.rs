use std::{marker::PhantomData, num::NonZeroU32};

use kithara_bufpool::HasPool;
use kithara_command::{Live, ScopedConfig};
use kithara_platform::{maybe_send::MaybeSend, sync::Arc};

#[cfg(feature = "offline")]
use crate::host::{
    HostConfig,
    offline::{OfflineRuntime, StartedOffline},
};
use crate::{
    HostCore, HostOwner, HostSettings, PlayError,
    rt::SessionOutput,
    session::{HostDispatcher, HostProtocol, HostRoot, RootView, web::WebSessionState},
    wasm::{HostReceiver, HostRoute, HostSender},
};

pub(in crate::host) struct Platform<S, O: HostOwner<S>> {
    pub(in crate::host) web_state: Option<WebSessionState<O>>,
    web_route: Option<Arc<HostRoute<O::Command>>>,
    marker: PhantomData<fn() -> S>,
}

type StartedPlatform<S, O> = (
    Arc<dyn HostDispatcher<<O as HostOwner<S>>::Command>>,
    Platform<S, O>,
);

impl<S, O: HostOwner<S>> Platform<S, O> {
    pub(in crate::host) fn realtime(
        root: HostRoot,
        view: RootView,
        _block: Option<NonZeroU32>,
        channel_config: ScopedConfig,
        output: SessionOutput,
        settings: Live<HostSettings, HostProtocol>,
        layer: impl FnOnce(HostCore<S, O::Deck>) -> O + MaybeSend + 'static,
    ) -> Result<StartedPlatform<S, O>, PlayError>
    where
        S: HasPool<f32> + Send + Sync + 'static,
    {
        let (dispatcher, state, route) = crate::session::web::spawn::<S, O>(
            root,
            view,
            channel_config,
            output,
            settings,
            layer,
        )?;
        Ok((
            dispatcher,
            Self {
                web_state: Some(state),
                web_route: Some(route),
                marker: PhantomData,
            },
        ))
    }
    #[cfg(feature = "offline")]
    pub(in crate::host) fn offline(
        config: HostConfig<S>,
        root: HostRoot,
        view: RootView,
        layer: impl FnOnce(HostCore<S, O::Deck>) -> O + MaybeSend + 'static,
    ) -> Result<StartedOffline<S, O>, PlayError>
    where
        S: HasPool<f32> + Send + Sync + 'static,
    {
        let (dispatcher, runtime) = OfflineRuntime::new(config, root, view, layer)?;
        Ok((
            dispatcher,
            Self {
                web_state: None,
                web_route: None,
                marker: PhantomData,
            },
            runtime,
        ))
    }
}

impl<S> crate::Host<S>
where
    S: HasPool<f32> + Send + Sync + 'static,
{
    pub(crate) fn browser_channel(&self) -> Result<(HostSender<S>, HostReceiver<S>), PlayError> {
        let platform = self._session.platform();
        let state = platform.web_state.as_ref().ok_or_else(|| {
            PlayError::Internal("worker routing requires a local browser host".to_owned())
        })?;
        let route = platform.web_route.as_ref().ok_or(PlayError::Closed)?;
        Ok((
            HostSender {
                id: self.id,
                root_view: self.root_view.clone(),
                postbox: route.postbox.clone(),
            },
            HostReceiver {
                state: state.clone(),
                route: route.clone(),
            },
        ))
    }

    pub(crate) fn browser_remote(sender: HostSender<S>) -> Self {
        Self {
            dispatcher: crate::session::web::remote::<S, HostCore<S>>(sender.postbox),
            id: sender.id,
            root_view: sender.root_view,
            _session: crate::host::owner::SessionRuntime::Realtime {
                _platform: Platform {
                    web_state: None,
                    web_route: None,
                    marker: PhantomData,
                },
            },
            owns_session: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        cell::{Cell, RefCell},
        num::NonZeroU32,
        rc::Rc,
        task::Waker,
    };

    use kithara_command::{Post, Ticket};
    use kithara_platform::{sync::Arc, time};
    use kithara_play::{PlayError, player::Player};
    use kithara_test_utils::{bufpool::TestPools, kithara};
    use kithara_warp::BeatGridId;

    use super::{Host, Platform, SessionRuntime, WorkerDecks, WorkerWake, spawn_clock};
    use crate::{
        HostSettings, consts,
        host::owner::SessionRoot,
        session::{
            HostCmd, HostDispatcher, HostRoot,
            protocol::{HostDispatchError, HostMailbox, HostPostbox, not_taken},
        },
    };

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Outcome {
        Ok,
        SessionGone,
        OtherError,
    }

    /// A deck that closes with a chosen outcome and counts its drains, ticks
    /// and drops.
    struct DeckProbe {
        close: Outcome,
        drains: Rc<Cell<usize>>,
        drops: Rc<RefCell<usize>>,
        ticks: Rc<Cell<usize>>,
    }

    impl Drop for DeckProbe {
        fn drop(&mut self) {
            *self.drops.borrow_mut() += 1;
        }
    }

    impl Player for DeckProbe {
        fn close(&mut self) -> Result<(), PlayError> {
            match self.close {
                Outcome::Ok => Ok(()),
                Outcome::SessionGone => Err(PlayError::SessionGone {
                    reason: "fixture deck close",
                }),
                Outcome::OtherError => Err(PlayError::Internal("fixture deck close failed".into())),
            }
        }

        fn drain(&mut self) {
            self.drains.set(self.drains.get() + 1);
        }

        fn hold(&mut self, _waker: Waker) {}

        fn release(&mut self) {}

        fn tick(&mut self) -> Result<(), PlayError> {
            self.ticks.set(self.ticks.get() + 1);
            Ok(())
        }
    }

    fn deck(close: Outcome, drops: &Rc<RefCell<usize>>) -> Box<DeckProbe> {
        Box::new(DeckProbe {
            close,
            drains: Rc::default(),
            drops: Rc::clone(drops),
            ticks: Rc::default(),
        })
    }

    /// Answers each `Detach` it is posted with the chosen outcome.
    struct Dispatcher {
        detach: Outcome,
        root: RefCell<HostRoot>,
        postbox: HostPostbox<TestPools>,
        mailbox: RefCell<HostMailbox<TestPools>>,
    }

    impl HostDispatcher<TestPools> for Dispatcher {
        fn dispatch(
            &self,
            cmd: HostCmd<TestPools>,
        ) -> Result<Ticket<PlayError>, HostDispatchError> {
            if self.detach == Outcome::SessionGone {
                return Err(HostDispatchError::NotTaken(PlayError::SessionGone {
                    reason: "fixture detach",
                }));
            }
            let ticket = self.postbox.post(cmd).map_err(not_taken)?;
            for Post { command, answer } in self.mailbox.borrow_mut().drain() {
                let HostCmd::Detach { grid_id } = command else {
                    panic!("unexpected fixture Host command")
                };
                answer.answer(if self.detach == Outcome::OtherError {
                    Err(PlayError::Internal("fixture detach failed".into()))
                } else {
                    self.root.borrow_mut().detach(grid_id).map_err(Into::into)
                });
            }
            Ok(ticket)
        }
    }

    struct Fixture {
        host: Host<TestPools>,
        deck: BeatGridId,
        drops: Rc<RefCell<usize>>,
        ticks: Rc<Cell<usize>>,
    }

    fn fixture(close: Outcome, detach: Outcome) -> Fixture {
        let sample_rate = NonZeroU32::new(44_100).expect("fixture sample rate");
        let SessionRoot {
            id: host_id,
            mut root,
            view: root_view,
        } = Host::<TestPools>::session_root(
            HostSettings::builder().sample_rate(sample_rate).build(),
        )
        .expect("fixture Host session");
        let deck_id = BeatGridId::allocate().expect("fixture deck grid id");
        root.attach(deck_id).expect("fixture deck attachment");

        let (postbox, mailbox) = kithara_command::mailbox();
        let dispatcher: Arc<dyn HostDispatcher<TestPools>> = Arc::new(Dispatcher {
            detach,
            root: RefCell::new(root),
            postbox,
            mailbox: RefCell::new(mailbox),
        });
        let drops = Rc::default();
        let probe = deck(close, &drops);
        let ticks = Rc::clone(&probe.ticks);
        let decks = WorkerDecks::default();
        decks.borrow_mut().hold(deck_id, probe);
        let host = Host {
            root_view,
            dispatcher,
            id: host_id,
            owns_session: false,
            session: SessionRuntime::realtime(Platform::remote(decks)),
        };
        Fixture {
            host,
            deck: deck_id,
            drops,
            ticks,
        }
    }

    #[kithara::test(wasm, flash(false))]
    fn successful_remove_releases_the_deck() {
        let Fixture {
            host, deck, drops, ..
        } = fixture(Outcome::Ok, Outcome::Ok);

        host.remove_deck(deck).expect("remove deck");

        assert_eq!(*drops.borrow(), 1);
    }

    #[kithara::test(wasm, flash(false))]
    #[case::closing(Outcome::SessionGone, Outcome::Ok)]
    #[case::detaching(Outcome::Ok, Outcome::SessionGone)]
    fn session_gone_releases_the_deck(#[case] close: Outcome, #[case] detach: Outcome) {
        let Fixture {
            host, deck, drops, ..
        } = fixture(close, detach);

        assert!(matches!(
            host.remove_deck(deck),
            Err(PlayError::SessionGone { .. })
        ));
        assert_eq!(*drops.borrow(), 1);
    }

    #[kithara::test(wasm, flash(false))]
    fn other_errors_retain_the_deck() {
        for (close, detach) in [
            (Outcome::OtherError, Outcome::Ok),
            (Outcome::Ok, Outcome::OtherError),
        ] {
            let Fixture {
                host,
                deck,
                drops,
                ticks,
            } = fixture(close, detach);

            assert!(matches!(
                host.remove_deck(deck),
                Err(PlayError::Internal(_))
            ));
            assert_eq!(*drops.borrow(), 0);
            host.session
                .platform()
                .worker_decks()
                .expect("a Worker Host holds decks")
                .borrow_mut()
                .tick();
            assert_eq!(ticks.get(), 1, "the Host still holds the deck");
            drop(host);
            assert_eq!(*drops.borrow(), 0);
        }
    }

    #[kithara::test(wasm)]
    fn a_wake_drains_a_worker_deck_before_it_returns() {
        let decks = WorkerDecks::default();
        let probe = deck(Outcome::Ok, &Rc::default());
        let drains = Rc::clone(&probe.drains);
        let id = BeatGridId::allocate().expect("fixture deck grid id");
        let waker = WorkerWake::waker(&decks, id);
        decks.borrow_mut().hold(id, probe);
        assert_eq!(drains.get(), 1, "held, the deck runs what waited for it");

        waker.wake_by_ref();

        assert_eq!(
            drains.get(),
            2,
            "a caller on the Worker reads its answer right after it posts"
        );
    }

    #[kithara::test(wasm)]
    async fn the_worker_clock_ticks_a_held_deck_until_it_is_released() {
        let decks = WorkerDecks::default();
        spawn_clock(Rc::downgrade(&decks));
        let drops = Rc::default();
        let probe = deck(Outcome::Ok, &drops);
        let ticks = Rc::clone(&probe.ticks);
        let id = BeatGridId::allocate().expect("fixture deck grid id");

        decks.borrow_mut().hold(id, probe);
        while ticks.get() < 2 {
            time::sleep(consts::SESSION_PUMP_INTERVAL).await;
        }
        let released = decks
            .borrow_mut()
            .release(id)
            .expect("the Host takes the deck back");
        let at_release = ticks.get();
        time::sleep(consts::SESSION_PUMP_INTERVAL * 3).await;

        assert_eq!(
            ticks.get(),
            at_release,
            "a released deck is no longer ticked"
        );
        drop(released);
    }
}
