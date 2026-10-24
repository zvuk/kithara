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
    wasm_bindgen_test::wasm_bindgen_test_configure!(run_in_browser);

    use std::{
        num::NonZeroU32,
        sync::atomic::{AtomicUsize, Ordering},
        task::Waker,
    };

    use kithara_command::Ticket;
    use kithara_platform::{
        sync::{Arc, Mutex},
        time,
        tokio::{
            select,
            task::{JoinHandle, spawn},
        },
    };
    use kithara_play::{
        DeckControl, DeckPass, HostedDeck, Outbox, PlayError, PlayWorker, PlayWorkerConfig,
        TrackReceipt,
    };
    use kithara_render::rt::DeckMixerConfig;
    use kithara_test_utils::{
        bufpool::{TestPools, pools},
        kithara,
    };

    use crate::{
        Host, HostCommand, HostCore, HostOwned, consts,
        session::{
            offline::mock,
            protocol::{HostDispatchError, HostDispatcher, HostPostbox, not_taken},
            tests::graph::state_for,
            web::WebSessionState,
        },
        wasm::{HostRoute, HostSender},
    };

    type Command = HostCommand<TestPools, dyn HostedDeck<TestPools>>;

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
        detach: Outcome,
        drains: Arc<AtomicUsize>,
        drops: Arc<AtomicUsize>,
        ticks: Arc<AtomicUsize>,
        waker: Arc<Mutex<Option<Waker>>>,
        worker: PlayWorker<TestPools>,
    }

    impl Drop for DeckProbe {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::SeqCst);
        }
    }

    impl Outcome {
        fn result(self) -> Result<(), PlayError> {
            match self {
                Outcome::Ok => Ok(()),
                Outcome::SessionGone => Err(PlayError::SessionGone {
                    reason: "fixture deck close",
                }),
                Outcome::OtherError => Err(PlayError::Internal("fixture deck close failed".into())),
            }
        }
    }

    impl DeckControl for DeckProbe {
        type Control = Arc<Mutex<Option<Waker>>>;

        fn control(&self) -> Self::Control {
            Arc::clone(&self.waker)
        }
    }

    impl HostedDeck<TestPools> for DeckProbe {
        fn worker(&self) -> Option<&PlayWorker<TestPools>> {
            Some(&self.worker)
        }

        fn mixer_config(&self) -> DeckMixerConfig {
            DeckMixerConfig::default()
        }

        fn close(&mut self, _out: &mut Outbox<'_, TestPools>) -> Result<(), PlayError> {
            self.close.result()?;
            self.detach.result()
        }

        fn drain(&mut self, _pass: DeckPass<'_>, _out: &mut Outbox<'_, TestPools>) {
            self.drains.fetch_add(1, Ordering::SeqCst);
        }

        fn hold(&mut self, waker: Waker) {
            *self.waker.lock() = Some(waker);
        }

        fn release(&mut self) {
            self.waker.lock().take();
        }

        fn tick(&mut self, _pass: DeckPass<'_>, _out: &mut Outbox<'_, TestPools>) {
            self.ticks.fetch_add(1, Ordering::SeqCst);
        }

        fn settle(
            &mut self,
            _receipt: TrackReceipt<'_, TestPools>,
            _pass: DeckPass<'_>,
            _out: &mut Outbox<'_, TestPools>,
        ) {
        }
    }

    struct Dispatcher {
        postbox: HostPostbox<Command>,
        state: WebSessionState<HostCore<TestPools>>,
        route: Arc<HostRoute<Command>>,
        clock: JoinHandle<()>,
    }

    impl Drop for Dispatcher {
        fn drop(&mut self) {
            self.clock.abort();
            self.route.close();
            self.state.lock().take();
        }
    }

    impl HostDispatcher<Command> for Dispatcher {
        fn dispatch(&self, cmd: Command) -> Result<Ticket<PlayError>, HostDispatchError> {
            let ticket = self.postbox.post(cmd).map_err(not_taken)?;
            self.route
                .drain::<TestPools, HostCore<TestPools>>(&self.state);
            Ok(ticket)
        }

        fn shutdown(&self) {}
    }

    struct Fixture {
        host: Host<TestPools>,
        deck: HostOwned<DeckProbe>,
        owner: Arc<Dispatcher>,
        state: WebSessionState<HostCore<TestPools>>,
        route: Arc<HostRoute<Command>>,
        drains: Arc<AtomicUsize>,
        drops: Arc<AtomicUsize>,
        ticks: Arc<AtomicUsize>,
    }

    fn fixture(close: Outcome, detach: Outcome) -> Fixture {
        let sample_rate = NonZeroU32::new(44_100).expect("fixture sample rate");
        let session = state_for(sample_rate, mock::start);
        let root_view = session.root_view.clone();
        let id = session.root.id();
        let (postbox, mailbox) = kithara_command::mailbox();
        let route = Arc::new(HostRoute::new(postbox.clone(), mailbox));
        let state = Arc::new(Mutex::new(Some(HostCore::new(session, route.wake.clone()))));
        let owner = Arc::new(Dispatcher {
            postbox: postbox.clone(),
            state: state.clone(),
            route: route.clone(),
            clock: spawn(clock(state.clone(), route.clone())),
        });
        let mut host = Host::browser_remote(HostSender {
            id,
            root_view,
            postbox,
        });
        host.dispatcher = owner.clone();
        let drops = Arc::default();
        let probe = DeckProbe {
            close,
            detach,
            drains: Arc::default(),
            drops: Arc::clone(&drops),
            ticks: Arc::default(),
            waker: Arc::default(),
            worker: PlayWorker::new(PlayWorkerConfig::builder(pools()).build()),
        };
        let drains = Arc::clone(&probe.drains);
        let ticks = Arc::clone(&probe.ticks);
        let deck = host.insert(probe).expect("fixture deck registration");
        ticks.store(0, Ordering::SeqCst);
        Fixture {
            host,
            deck,
            owner,
            state,
            route,
            drains,
            drops,
            ticks,
        }
    }

    #[kithara::hang_watchdog]
    async fn clock(state: WebSessionState<HostCore<TestPools>>, route: Arc<HostRoute<Command>>) {
        loop {
            hang_tick!();
            select! {
                _ = route.wake.notified() => {},
                _ = time::sleep(consts::SESSION_PUMP_INTERVAL) => {},
            }
            if !route.drain::<TestPools, HostCore<TestPools>>(&state) {
                break;
            }
            hang_reset!();
        }
    }

    #[kithara::test(flash(false))]
    fn successful_remove_releases_the_deck() {
        let Fixture {
            mut host,
            deck,
            drops,
            owner: _owner,
            ..
        } = fixture(Outcome::Ok, Outcome::Ok);

        host.remove(&deck).expect("remove deck");

        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[kithara::test(flash(false))]
    #[case::closing(Outcome::SessionGone, Outcome::Ok)]
    #[case::detaching(Outcome::Ok, Outcome::SessionGone)]
    fn session_gone_releases_the_deck(#[case] close: Outcome, #[case] detach: Outcome) {
        let Fixture {
            mut host,
            deck,
            drops,
            owner: _owner,
            ..
        } = fixture(close, detach);

        assert!(matches!(
            host.remove(&deck),
            Err(PlayError::SessionGone { .. })
        ));
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[kithara::test(flash(false))]
    #[kithara::hang_watchdog]
    fn other_errors_retain_the_deck() {
        for (close, detach) in [
            (Outcome::OtherError, Outcome::Ok),
            (Outcome::Ok, Outcome::OtherError),
        ] {
            hang_tick!();
            let Fixture {
                mut host,
                deck,
                drops,
                ticks,
                owner: _owner,
                ..
            } = fixture(close, detach);

            assert!(matches!(host.remove(&deck), Err(PlayError::Internal(_))));
            assert_eq!(drops.load(Ordering::SeqCst), 0);
            assert!(host.root_view.holds(deck.id()));
            assert_eq!(
                ticks.load(Ordering::SeqCst),
                1,
                "the Host still holds the deck"
            );
            drop(host);
            assert_eq!(drops.load(Ordering::SeqCst), 0);
            hang_reset!();
        }
    }

    #[kithara::test]
    fn a_wake_drains_a_worker_deck_before_it_returns() {
        let Fixture {
            deck,
            drains,
            state,
            route,
            owner: _owner,
            ..
        } = fixture(Outcome::Ok, Outcome::Ok);
        let waker = deck.control().lock().clone().expect("held deck waker");
        assert_eq!(
            drains.load(Ordering::SeqCst),
            1,
            "held, the deck runs what waited for it"
        );

        waker.wake_by_ref();
        assert_eq!(
            drains.load(Ordering::SeqCst),
            1,
            "the wake queues work for the owner"
        );
        route.drain::<TestPools, HostCore<TestPools>>(&state);

        assert_eq!(
            drains.load(Ordering::SeqCst),
            2,
            "a caller on the Worker reads its answer at the owner's next turn"
        );
        route.drain::<TestPools, HostCore<TestPools>>(&state);
        assert_eq!(
            drains.load(Ordering::SeqCst),
            2,
            "one wake queues exactly one drain"
        );
    }

    #[kithara::test]
    async fn the_worker_clock_ticks_a_held_deck_until_it_is_released() {
        let Fixture {
            mut host,
            deck: released,
            ticks,
            owner: _owner,
            ..
        } = fixture(Outcome::Ok, Outcome::Ok);
        wait_for_ticks(&ticks).await;
        host.remove(&released)
            .expect("the Host takes the deck back");
        let at_release = ticks.load(Ordering::SeqCst);
        time::sleep(consts::SESSION_PUMP_INTERVAL * 3).await;

        assert_eq!(
            ticks.load(Ordering::SeqCst),
            at_release,
            "a released deck is no longer ticked"
        );
        drop(released);
    }

    #[kithara::hang_watchdog]
    async fn wait_for_ticks(ticks: &AtomicUsize) {
        let mut seen = ticks.load(Ordering::SeqCst);
        while ticks.load(Ordering::SeqCst) < 2 {
            hang_tick!();
            time::timeout(
                __hang_detector.remaining(),
                time::sleep(consts::SESSION_PUMP_INTERVAL),
            )
            .await
            .expect("owner clock watchdog");
            if ticks.load(Ordering::SeqCst) != seen {
                seen = ticks.load(Ordering::SeqCst);
                hang_reset!();
            }
        }
    }
}
