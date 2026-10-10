#![cfg(not(target_arch = "wasm32"))]

use std::{
    num::{NonZeroU32, NonZeroUsize},
    task::Waker,
};

use delegate::delegate;
use kithara::{
    host::{HostConfig, HostOwned, HostSettings},
    platform::{
        sync::{Arc, Mutex},
        thread,
    },
    play::{
        BufferGeometryError, DeckControl, DeckMixerConfig, DeckPass, HostedDeck, Outbox, PlayError,
        PlayWorker, PlayWorkerConfig, ResourcePrep, SessionError, SessionEvent, TrackReceipt,
    },
    queue::{Queue, QueueConfig},
    warp::WarpConfig,
    worker::DispatcherConfig,
};
use kithara_integration_tests::{offline::OfflineHostHarness, smoothing::consts};
use kithara_test_utils::bufpool::{Pools, TestPools, pools};

/// An offline Host at the suite's rate and block, drawing on `region`.
async fn offline_host(region: &Pools) -> OfflineHostHarness<TestPools> {
    let config = HostConfig::offline(region.clone())
        .settings(HostSettings::builder().sample_rate(sample_rate()).build())
        .max_block_frames(block_frames())
        .build();
    OfflineHostHarness::new(config).await.expect("offline host")
}

fn block_frames() -> NonZeroU32 {
    u32::try_from(consts::BLOCK_FRAMES)
        .ok()
        .and_then(NonZeroU32::new)
        .expect("block size")
}

fn sample_rate() -> NonZeroU32 {
    NonZeroU32::new(consts::SAMPLE_RATE).expect("sample rate")
}

#[kithara::test(tokio)]
async fn failed_deck_preparation_releases_host_membership() {
    let region = pools();
    let host = offline_host(&region).await;
    let worker = PlayWorker::new(PlayWorkerConfig::builder(region).build());
    let invalid = Queue::new(
        QueueConfig::builder()
            .prep(
                ResourcePrep::builder()
                    .worker(worker.clone())
                    .warp(
                        WarpConfig::builder()
                            .render_quantum_frames(NonZeroUsize::new(32).expect("quantum"))
                            .build(),
                    )
                    .response_budget_frames(NonZeroUsize::new(1).expect("budget"))
                    .build(),
            )
            .build(),
    );
    assert!(matches!(
        host.insert(invalid).await,
        Err(PlayError::Session(SessionError::BufferGeometry(
            BufferGeometryError::BudgetExceeded { .. }
        )))
    ));
    host.with(|host| {
        assert!(host.is_empty());
        assert!(
            host.output_sample_rate().measured.is_none(),
            "failed preparation must close an otherwise idle stream"
        );
    })
    .await;
    let valid = Queue::new(
        QueueConfig::builder()
            .prep(ResourcePrep::builder().worker(worker).build())
            .build(),
    );
    let deck = host
        .insert(valid)
        .await
        .expect("host can prepare the next deck");
    let control = deck.control().clone();
    host.run(move || control.set_eq_gain(0, -6.0).expect("configure idle EQ"))
        .await;
    host.render(consts::BLOCK_FRAMES).await;
    host.with(move |host| {
        assert_eq!(deck.eq_gain(0), Some(-6.0));
        deck.play();
        assert!(host.output_sample_rate().measured.is_some());
        assert_eq!(deck.eq_gain(0), Some(-6.0));
        deck.pause();
    })
    .await;
    host.close().await;
}

/// The Host starts a deck as it takes it and stops it as it hands it back, so
/// the deck's output runs for its whole membership with nothing played.
#[kithara::test(tokio)]
async fn a_deck_runs_from_its_insert_to_its_remove() {
    let region = pools();
    let host = offline_host(&region).await;
    let worker = PlayWorker::new(PlayWorkerConfig::builder(region).build());
    let deck = host
        .insert(Queue::new(
            QueueConfig::builder()
                .prep(ResourcePrep::builder().worker(worker).build())
                .build(),
        ))
        .await
        .expect("the Host takes the deck");
    host.with(move |host| {
        assert!(
            host.output_sample_rate().measured.is_some(),
            "the Host starts a deck as it takes it"
        );
        host.remove(&deck).expect("the Host hands the deck back");
        assert!(host.is_empty());
        assert!(
            host.output_sample_rate().measured.is_none(),
            "the last deck the Host hands back takes the output with it"
        );
    })
    .await;
    host.close().await;
}

#[kithara::test(tokio)]
async fn a_route_change_reaches_every_deck_the_host_holds() {
    let region = pools();
    let host = offline_host(&region).await;
    let worker = PlayWorker::new(PlayWorkerConfig::builder(region).build());
    let mut decks = Vec::new();
    for _ in 0..2 {
        let deck = host
            .insert(Queue::new(
                QueueConfig::builder()
                    .prep(ResourcePrep::builder().worker(worker.clone()).build())
                    .build(),
            ))
            .await
            .expect("the Host takes the deck");
        decks.push((deck.subscribe::<SessionEvent>(), deck));
    }

    host.invalidate_audio_route("oldDeviceUnavailable")
        .await
        .expect("the Host restarts its route");

    for (heard, _deck) in &mut decks {
        assert!(
            matches!(
                heard.try_recv().map(|envelope| envelope.event),
                Ok(SessionEvent::RouteChanged { .. })
            ),
            "every deck the Host holds hears the route change"
        );
    }
    host.close().await;
}

/// A close, a drain or a tick of a deck, with the thread it ran on.
#[derive(Clone, Debug, PartialEq)]
enum Call {
    Close(Option<String>),
    Drain(Option<String>),
    Tick(Option<String>),
}

/// A player that notes each close, drain and tick of the player it wraps, in
/// order.
struct ThreadProbe<P> {
    inner: P,
    seen: Arc<Mutex<Vec<Call>>>,
}

impl<P> ThreadProbe<P> {
    fn note(&self, call: fn(Option<String>) -> Call) {
        self.seen
            .lock()
            .push(call(thread::current().name().map(str::to_owned)));
    }
}

impl<S, P: HostedDeck<S>> HostedDeck<S> for ThreadProbe<P> {
    fn close(&mut self, out: &mut Outbox<'_, S>) -> Result<(), PlayError> {
        self.note(Call::Close);
        self.inner.close(out)
    }

    fn drain(&mut self, pass: DeckPass<'_>, out: &mut Outbox<'_, S>) {
        self.note(Call::Drain);
        self.inner.drain(pass, out);
    }

    fn tick(&mut self, pass: DeckPass<'_>, out: &mut Outbox<'_, S>) {
        self.note(Call::Tick);
        self.inner.tick(pass, out);
    }

    delegate! {
        to self.inner {
            fn worker(&self) -> Option<&PlayWorker<S>>;
            fn mixer_config(&self) -> DeckMixerConfig;
            fn settle(&mut self, receipt: TrackReceipt<'_, S>, pass: DeckPass<'_>, out: &mut Outbox<'_, S>);
            fn hold(&mut self, waker: Waker);
            fn release(&mut self);
        }
    }
}

impl<P: DeckControl> DeckControl for ThreadProbe<P> {
    type Control = P::Control;

    fn control(&self) -> Self::Control {
        self.inner.control()
    }
}

/// The name of the session thread a probed Host runs.
const SESSION: &str = "deck-session";

/// An offline Host whose session thread is named [`SESSION`], holding a probe
/// that notes into `seen` each call its session makes on a bare player.
async fn probed_host(
    seen: &Arc<Mutex<Vec<Call>>>,
) -> (
    OfflineHostHarness<TestPools>,
    HostOwned<ThreadProbe<Queue<TestPools>>>,
) {
    let region = pools();
    let config = HostConfig::offline(region.clone())
        .settings(HostSettings::builder().sample_rate(sample_rate()).build())
        .max_block_frames(block_frames())
        .dispatcher(
            DispatcherConfig::builder()
                .name(SESSION)
                .capacity(NonZeroUsize::MIN)
                .build(),
        )
        .build();
    let host = OfflineHostHarness::new(config).await.expect("offline host");
    let deck = host
        .insert(ThreadProbe {
            inner: Queue::new(
                QueueConfig::builder()
                    .prep(
                        ResourcePrep::builder()
                            .worker(PlayWorker::new(PlayWorkerConfig::builder(region).build()))
                            .build(),
                    )
                    .build(),
            ),
            seen: Arc::clone(seen),
        })
        .await
        .expect("the Host takes the deck");
    (host, deck)
}

/// The Host's session thread holds its decks: it runs a deck's commands as it
/// takes the deck and ticks the deck once ahead of each block it renders.
#[kithara::test(tokio)]
async fn the_session_thread_drains_and_ticks_the_decks_it_holds() {
    const BLOCKS: usize = 3;
    let seen = Arc::default();
    let (host, _deck) = probed_host(&seen).await;
    host.render(consts::BLOCK_FRAMES * BLOCKS).await;

    let session = || Some(SESSION.to_owned());
    let mut expected = vec![Call::Drain(session())];
    expected.extend(vec![Call::Tick(session()); BLOCKS]);
    assert_eq!(
        *seen.lock(),
        expected,
        "a drain as the session thread takes the deck, then one tick ahead of each block"
    );
    host.close().await;
}

/// A deck the Host hands back is closed where it is held: on the session
/// thread, which runs every other command to it.
#[kithara::test(tokio)]
async fn removing_a_deck_closes_it_on_the_session_thread() {
    let seen = Arc::default();
    let (host, deck) = probed_host(&seen).await;

    host.with(move |host| host.remove(&deck))
        .await
        .expect("the Host hands the deck back");

    let closes: Vec<_> = seen
        .lock()
        .iter()
        .filter(|call| matches!(call, Call::Close(_)))
        .cloned()
        .collect();
    assert_eq!(
        closes,
        vec![Call::Close(Some(SESSION.to_owned()))],
        "the session thread closes the deck, once"
    );
    host.close().await;
}
