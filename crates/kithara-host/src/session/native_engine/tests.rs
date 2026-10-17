#[cfg(feature = "offline")]
use kithara_command::{Seq, When};
#[cfg(feature = "offline")]
use kithara_effects::LimiterConfig;
#[cfg(feature = "offline")]
use kithara_platform::{thread::JoinHandle, time::Duration};
#[cfg(feature = "offline")]
use kithara_play::{DeckPass, HostedDeck, Outbox};
#[cfg(feature = "offline")]
use kithara_signal::{FrameCount, SessionFrame};
use kithara_test_utils::kithara;
#[cfg(feature = "offline")]
use kithara_test_utils::{bufpool::TestPools, wait_until};
#[cfg(feature = "offline")]
use kithara_warp::BeatGridState;

#[cfg(feature = "offline")]
use super::*;
#[cfg(feature = "offline")]
use crate::{
    DeckId, HostCommand, HostSettingsChange, HostSettingsExec, HostSettled, MetronomeConfigChange,
    session::{
        protocol::ask as dispatch,
        tests::{
            deck_probe::{Seen, next, probe, so_far, ticks},
            graph::empty_root,
        },
    },
};

#[cfg(feature = "offline")]
type BaseCommand = HostCommand<TestPools, dyn HostedDeck<TestPools>>;
#[cfg(feature = "offline")]
enum Command {
    Host(BaseCommand),
    Render {
        position: u64,
        reply: mpsc::Sender<Vec<f32>>,
    },
}
#[cfg(feature = "offline")]
impl From<BaseCommand> for Command {
    fn from(command: BaseCommand) -> Self {
        Self::Host(command)
    }
}
#[cfg(feature = "offline")]
struct RigOwner(HostCore<TestPools>);
#[cfg(feature = "offline")]
impl HostSettingsExec<()> for RigOwner {
    type At = When<SessionFrame>;
    type Output = Result<Option<Seq>, PlayError>;

    delegate::delegate! {
        to self.0 {
            fn exec_sample_rate(
                &mut self,
                value: NonZeroU32,
                at: Self::At,
                cx: &mut (),
            ) -> Self::Output;
            fn exec_tempo(
                &mut self,
                value: crate::api::Tempo,
                at: Self::At,
                cx: &mut (),
            ) -> Self::Output;
            fn exec_live(
                &mut self,
                change: HostSettingsChange,
                at: Self::At,
                cx: &mut (),
            ) -> Self::Output;
        }
    }
}
#[cfg(feature = "offline")]
impl HostOwner<TestPools> for RigOwner {
    type Command = Command;
    type Deck = dyn HostedDeck<TestPools>;
    fn apply(&mut self, command: Command) -> Result<Option<Seq>, PlayError> {
        match command {
            Command::Host(command) => self.0.apply(command),
            Command::Render { position, reply } => {
                self.0.prepare_offline()?;
                let mut output = vec![0.0; 1_024];
                self.0.render_offline(position, 512, &mut output)?;
                reply.send(output).map_err(|_| PlayError::Closed)?;
                Ok(None)
            }
        }
    }

    delegate::delegate! {
        to self.0 {
            fn register(&mut self, id: DeckId, deck: Box<Self::Deck>) -> Result<(), PlayError>;
            fn each_deck(
                &mut self,
                visit: &mut dyn FnMut(
                    DeckId,
                    &mut Self::Deck,
                    &mut Outbox<'_, TestPools>,
                    DeckPass<'_>,
                ),
            );
            fn with_deck(
                &mut self,
                id: DeckId,
                visit: &mut dyn FnMut(&mut Self::Deck, &mut Outbox<'_, TestPools>, DeckPass<'_>),
            ) -> Result<(), PlayError>;
            fn prepare_offline(&mut self) -> Result<(), PlayError>;
            fn render_offline(
                &mut self,
                position: u64,
                frames: usize,
                output: &mut [f32],
            ) -> Result<(), PlayError>;
            fn transport(&mut self) -> Option<crate::api::SessionTransportSnapshot>;
            fn begin_pass(&mut self);
            fn pass(&mut self) -> Vec<HostSettled>;
            fn clock(&self) -> Option<(SessionFrame, FrameCount)>;
            fn host_room(&self) -> usize;
        }
    }

    fn release_id(command: &Command) -> Option<DeckId> {
        match command {
            Command::Host(command) => HostCore::<TestPools>::release_id(command),
            Command::Render { .. } => None,
        }
    }
    fn is_next_tempo(command: &Command) -> bool {
        match command {
            Command::Host(command) => HostCore::<TestPools>::is_next_tempo(command),
            Command::Render { .. } => false,
        }
    }
}

#[cfg(feature = "offline")]
fn deck_session() -> (Arc<SessionClient<Command>>, JoinHandle<()>, RootView) {
    let sample_rate = NonZeroU32::new(48_000).expect("test sample rate");
    let (cmd_tx, cmd_rx) = mpsc::channel();
    let (postbox, mut mailbox) = mailbox();
    mailbox.hold(Waker::from(Arc::new(SessionWake(Mutex::new(
        cmd_tx.clone(),
    )))));
    let client = Arc::new(SessionClient {
        postbox,
        cmd_tx: Mutex::new(cmd_tx),
    });
    let inbox: Arc<dyn DeckInbox> = client.clone();
    let (ready, started) = mpsc::channel();
    let worker = spawn_named(consts::DECK_SESSION, move || {
        let (root, view) = empty_root(sample_rate);
        let state = SessionState::new(
            root,
            view.clone(),
            SessionBufferConfig::default(),
            SessionOutput::new(LimiterConfig::default()),
            Live::new(HostSettings::builder().sample_rate(sample_rate).build())
                .expect("fixture settings"),
            crate::HostConfig::<TestPools>::builder()
                .build()
                .channel_config(),
            crate::session::offline::mock::start,
        );
        ready.send(view).expect("owner thread publishes its view");
        engine_thread(&cmd_rx, mailbox, RigOwner(HostCore::new(state, inbox)));
    });
    let view = receive(&started, "owner startup").expect("owner thread starts");
    (client, worker, view)
}

#[cfg(feature = "offline")]
#[kithara_test_utils::kithara::hang_watchdog(ctx = String, timeout = kithara_test_utils::hang::default_timeout().min(Duration::from_secs(4)))]
fn shut_down(client: &SessionClient<Command>, worker: JoinHandle<()>) {
    client.shutdown();
    while !worker.is_finished() {
        hang_park!(
            |remaining: Duration| {
                kithara_platform::thread::sleep(remaining.min(Duration::from_millis(1)));
            },
            "native session thread has not exited".to_owned()
        );
    }
    hang_reset!();
    assert!(worker.join().is_ok());
}

#[cfg(feature = "offline")]
fn ask(client: &SessionClient<Command>, command: Command) -> Result<(), &'static str> {
    let command_name = match &command {
        Command::Host(command) => match command {
            HostCommand::Register { .. } => "Register",
            HostCommand::Configure(..) => "Configure",
            HostCommand::Close(_) => "Close",
            HostCommand::Release(_) => "Release",
            HostCommand::AttachOutputs { .. } => "AttachOutputs",
            HostCommand::DetachOutputs { .. } => "DetachOutputs",
            HostCommand::Restart => "Restart",
            HostCommand::_Marker(_) => "_Marker",
        },
        Command::Render { .. } => "Render",
    };
    dispatch(client, command, command_name).map_err(|_| "native command refused")
}

#[cfg(feature = "offline")]
fn render(client: &SessionClient<Command>, position: u64) -> Vec<f32> {
    let (reply, receiver) = mpsc::channel();
    ask(client, Command::Render { position, reply }).expect("native owner renders");
    receive(&receiver, "rendered block").expect("rendered block")
}

/// Commands posted before the deck was held woke no one, so the session
/// thread runs them as soon as it holds the deck.
#[kithara::test]
#[cfg(feature = "offline")]
fn the_session_thread_drains_a_deck_as_it_takes_it() {
    let (client, worker, _view) = deck_session();
    let (id, deck, seen) = probe();

    ask(&client, Command::Host(HostCommand::Register { id, deck }))
        .expect("the session takes the deck");

    assert!(matches!(next(&seen), Seen::Held(_)));
    assert!(
        matches!(next(&seen), Seen::Drained(thread) if thread.as_deref() == Some(consts::DECK_SESSION)),
        "the session thread drains a deck it takes before ticking it"
    );
    shut_down(&client, worker);
}

#[kithara::test]
#[cfg(feature = "offline")]
#[kithara_test_utils::kithara::hang_watchdog(ctx = String, timeout = kithara_test_utils::hang::default_timeout().min(Duration::from_secs(4)))]
fn the_session_thread_ticks_a_held_deck_until_it_is_released() {
    let (client, worker, _view) = deck_session();
    let (id, deck, seen) = probe();

    ask(&client, Command::Host(HostCommand::Register { id, deck }))
        .expect("the session takes the deck");
    let mut ticked = 0;
    while ticked < 2 {
        hang_tick!(format!("held deck ticks={ticked}/2"));
        if let Seen::Ticked(thread) = next(&seen) {
            assert_eq!(thread.as_deref(), Some(consts::DECK_SESSION));
            ticked += 1;
            hang_reset!();
        }
    }
    // The session hands the deck back by value: nothing it does later
    // reaches it, so the record ends with the hand-back.
    ask(&client, Command::Host(HostCommand::Release(id)))
        .expect("the session closes the deck scope");

    let seen = so_far(&seen);
    let at = seen
        .iter()
        .position(|seen| matches!(seen, Seen::Released))
        .expect("the deck comes back released");
    assert_eq!(ticks(&seen[at..]), 0, "a released deck is no longer ticked");
    shut_down(&client, worker);
}

#[kithara::test]
#[cfg(feature = "offline")]
fn a_deck_the_session_lets_go_is_released_before_it_is_handed_back() {
    let (client, worker, _view) = deck_session();
    let (id, deck, seen) = probe();
    ask(&client, Command::Host(HostCommand::Register { id, deck }))
        .expect("the session takes the deck");

    ask(&client, Command::Host(HostCommand::Release(id)))
        .expect("the session closes the deck scope");

    assert!(
        so_far(&seen)
            .iter()
            .any(|seen| matches!(seen, Seen::Released)),
        "a deck comes back released, so nothing waits on what it had queued"
    );
    shut_down(&client, worker);
}

/// The session lets go of its decks on its own thread as it shuts down:
/// each is released, then dropped, before the shutdown answers.
#[kithara::test]
#[cfg(feature = "offline")]
fn shutdown_lets_go_of_every_deck_before_it_answers() {
    let (client, worker, _view) = deck_session();
    let (id, deck, seen) = probe();
    ask(&client, Command::Host(HostCommand::Register { id, deck }))
        .expect("the session takes the deck");

    shut_down(&client, worker);

    let after = so_far(&seen);
    let released = after.iter().position(|seen| matches!(seen, Seen::Released));
    let dropped = after.iter().position(|seen| matches!(seen, Seen::Dropped));
    assert!(
        matches!((released, dropped), (Some(released), Some(dropped)) if released < dropped),
        "a deck is released, then dropped"
    );
}

#[kithara::test]
#[cfg(feature = "offline")]
#[kithara_test_utils::kithara::hang_watchdog(ctx = String, timeout = kithara_test_utils::hang::default_timeout().min(Duration::from_secs(4)))]
fn a_woken_deck_drains_on_the_session_thread() {
    let (client, worker, _view) = deck_session();
    let (id, deck, seen) = probe();
    ask(&client, Command::Host(HostCommand::Register { id, deck }))
        .expect("the session takes the deck");
    let Seen::Held(waker) = next(&seen) else {
        panic!("the session holds the deck before anything else");
    };
    assert!(
        matches!(next(&seen), Seen::Drained(_)),
        "drained as it is held"
    );

    waker.wake_by_ref();

    loop {
        hang_tick!("woken deck has not drained".to_owned());
        match next(&seen) {
            Seen::Drained(thread) => {
                assert_eq!(thread.as_deref(), Some(consts::DECK_SESSION));
                break;
            }
            Seen::Ticked(_) => {}
            _ => panic!("a woken deck is drained while it stays held"),
        }
    }
    shut_down(&client, worker);
}

#[kithara::test]
fn output_block_override_preserves_the_backend_default_or_sets_128() {
    use super::{NonZeroU32, cpal_config};

    let inherited = cpal_config(44_100, None);
    assert_eq!(
        inherited.output.desired_block_frames,
        firewheel::cpal::CpalOutputConfig::default().desired_block_frames
    );

    let frames = NonZeroU32::new(128).expect("test block size is non-zero");
    let configured = cpal_config(44_100, Some(frames));
    assert_eq!(configured.output.desired_block_frames, Some(128));
}

#[kithara::test]
#[cfg(feature = "offline")]
#[kithara_test_utils::kithara::hang_watchdog(ctx = String, timeout = kithara_test_utils::hang::default_timeout().min(Duration::from_secs(4)))]
fn idle_native_worker_publishes_rendered_transport_without_a_deck_tick() {
    let runtime = kithara_platform::tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("test wait runtime");
    // The test thread drives every render. Inside a participating poll it
    // holds the virtual clock while it works, so the clock moves only on the
    // waits' own poll ticks, never while the driver is merely busy or preempted.
    runtime.block_on(kithara_platform::flash::participate(
        async {
            let (client, worker, root_view) = deck_session();
            let (id, deck, _seen) = probe();
            ask(&client, Command::Host(HostCommand::Register { id, deck })).expect("fixture deck");
            let _ = render(&client, 0);
            let mut clock_samples = 512;
            wait_until(
                Duration::from_secs(2),
                "native Host delivers graph change",
                || {
                    hang_tick!(format!(
                        "native transport grid={:?}; clock={clock_samples}",
                        root_view.grid().state()
                    ));
                    let _ = render(&client, clock_samples);
                    clock_samples += 512;
                    root_view.grid().state() == BeatGridState::Live
                },
            )
            .await
            .expect("the transport's own tempo reaches the read-only Host view without a command");
            hang_reset!();

            assert!(matches!(
                ask(
                    &client,
                    Command::Host(HostCommand::Configure(
                        HostSettingsChange::Metronome(MetronomeConfigChange::Enabled(true)),
                        When::Next
                    ))
                ),
                Ok(())
            ));
            let mut on_blocks = 0;
            wait_until(
                Duration::from_secs(3),
                "idle native metronome sounds",
                || {
                    let mut sounded = false;
                    for _ in 0..100 {
                        hang_tick!(format!(
                            "metronome enabled; blocks={on_blocks}; clock={clock_samples}"
                        ));
                        let pcm = render(&client, clock_samples);
                        clock_samples += 512;
                        on_blocks += 1;
                        sounded |= pcm.iter().any(|sample| sample.abs() > 0.01);
                    }
                    sounded
                },
            )
            .await
            .unwrap_or_else(|error| {
                panic!("metronome on reaches PCM: {error}; blocks={on_blocks}")
            });
            hang_reset!();

            assert!(matches!(
                ask(
                    &client,
                    Command::Host(HostCommand::Configure(
                        HostSettingsChange::Metronome(MetronomeConfigChange::Enabled(false)),
                        When::Next
                    ))
                ),
                Ok(())
            ));
            wait_until(
                Duration::from_secs(4),
                "idle native metronome stays off",
                || {
                    let mut silent = true;
                    for _ in 0..100 {
                        hang_tick!(format!("metronome disabled; clock={clock_samples}"));
                        let pcm = render(&client, clock_samples);
                        clock_samples += 512;
                        silent &= pcm.iter().all(|sample| sample.abs() < 1e-6);
                    }
                    silent
                },
            )
            .await
            .expect("the muted capture spans more than one beat");
            hang_reset!();
            shut_down(&client, worker);
        },
        std::panic::Location::caller(),
    ));
}

#[kithara_test_utils::kithara::hang_watchdog(ctx = String, timeout = kithara_test_utils::hang::default_timeout().min(Duration::from_secs(4)))]
fn receive<T>(receiver: &mpsc::Receiver<T>, reason: &str) -> Result<T, mpsc::RecvTimeoutError> {
    loop {
        let result = receiver.recv_timeout(Instant::now() + __hang_detector.remaining());
        if !matches!(result, Err(mpsc::RecvTimeoutError::Timeout)) {
            hang_reset!();
            return result;
        }
        hang_tick!(reason.to_owned());
    }
}
