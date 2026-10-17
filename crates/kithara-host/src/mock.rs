#[cfg(test)]
pub(crate) use fixture::*;

#[cfg(test)]
mod fixture {
    use std::num::NonZeroU32;

    use firewheel::FirewheelContext;
    use kithara_command::{Live, When};
    use kithara_effects::LimiterConfig;
    use kithara_platform::{sync::Arc, time::Duration};
    use kithara_play::{HostedDeck, PlayError, PlayWorker, PlayWorkerConfig, ResourcePrep};
    use kithara_queue::{Queue, QueueConfig};
    use kithara_test_utils::{
        bufpool::{TestPools, pools},
        kithara,
    };
    use kithara_warp::BeatGridId;

    use crate::{
        HostCommand, HostCore, HostOwner, HostSettings,
        rt::SessionOutput,
        session::{
            decks::{DeckInbox, DeckMsg},
            state::{HostRoot, RootView, SessionBufferConfig, SessionState, SessionStream},
        },
    };

    fn session_state(
        start: impl FnMut(&mut FirewheelContext, u32) -> Result<SessionStream, String> + Send + 'static,
    ) -> SessionState<SessionStream, TestPools> {
        let rate = NonZeroU32::new(44_100).expect("fixture rate");
        let root = HostRoot::new(BeatGridId::allocate().expect("fixture grid id"), rate);
        let settings = HostSettings::builder().sample_rate(rate).build();
        let root_view = RootView::new(&root, settings);
        SessionState::new(
            root,
            root_view,
            SessionBufferConfig::default(),
            SessionOutput::new(LimiterConfig::default()),
            Live::new(settings).expect("fixture settings"),
            crate::HostConfig::<TestPools>::builder()
                .settings(settings)
                .build()
                .channel_config(),
            start,
        )
    }

    pub(crate) fn registration(id: BeatGridId) -> Command {
        let prep = ResourcePrep::builder()
            .worker(PlayWorker::new(PlayWorkerConfig::builder(pools()).build()))
            .build();
        HostCommand::Register {
            id,
            deck: Box::new(Queue::new(QueueConfig::builder().prep(prep).build())),
        }
    }

    pub(crate) type Command = HostCommand<TestPools, Queue<TestPools>>;

    pub(crate) struct Inbox(
        pub(crate) kithara_platform::sync::Mutex<kithara_platform::sync::mpsc::Sender<DeckMsg>>,
    );
    impl DeckInbox for Inbox {
        #[kithara::hang_watchdog(timeout = Duration::from_secs(1))]
        fn post(&self, message: DeckMsg) -> Result<(), PlayError> {
            hang_tick!();
            let result = self.0.lock().send(message).map_err(|_| PlayError::Closed);
            hang_tick!();
            result
        }
    }

    pub(crate) struct TestState<D: HostedDeck<TestPools> = Queue<TestPools>> {
        pub(crate) owner: HostCore<TestPools, D>,
        pub(crate) messages: kithara_platform::sync::mpsc::Receiver<DeckMsg>,
    }
    impl<D: HostedDeck<TestPools>> std::ops::Deref for TestState<D> {
        type Target = SessionState<SessionStream, TestPools>;
        fn deref(&self) -> &Self::Target {
            &self.owner.session
        }
    }
    impl<D: HostedDeck<TestPools>> std::ops::DerefMut for TestState<D> {
        fn deref_mut(&mut self) -> &mut Self::Target {
            &mut self.owner.session
        }
    }

    pub(crate) fn test_state(
        start: impl FnMut(&mut FirewheelContext, u32) -> Result<SessionStream, String> + Send + 'static,
    ) -> TestState {
        TestState::new(start)
    }

    impl<D: HostedDeck<TestPools>> TestState<D> {
        pub(crate) fn new(
            start: impl FnMut(&mut FirewheelContext, u32) -> Result<SessionStream, String>
            + Send
            + 'static,
        ) -> Self {
            let state = session_state(start);
            let (tx, messages) = kithara_platform::sync::mpsc::channel();
            let inbox = Arc::new(Inbox(kithara_platform::sync::Mutex::new(tx)));
            Self {
                owner: HostCore::new(state, inbox),
                messages,
            }
        }
    }

    #[kithara::hang_watchdog(timeout = Duration::from_secs(1))]
    pub(crate) fn ask<D: HostedDeck<TestPools>>(
        state: &mut TestState<D>,
        command: HostCommand<TestPools, D>,
    ) -> Result<(), PlayError> {
        kithara_platform::flash::participate_sync(|| {
            hang_tick!();
            state.owner.begin_pass();
            for message in state.messages.try_iter() {
                hang_tick!();
                message.run(&mut state.owner);
                hang_tick!();
                hang_reset!();
            }
            let result = state.owner.apply(command);
            hang_tick!();
            result?;
            state.owner.pass();
            state.owner.begin_pass();
            state.owner.pass();
            hang_tick!();
            Ok(())
        })
    }

    #[kithara::hang_watchdog(timeout = Duration::from_secs(1))]
    pub(crate) fn render_block<D: HostedDeck<TestPools>>(
        state: &mut TestState<D>,
        clock: u64,
    ) -> Vec<f32> {
        kithara_platform::flash::participate_sync(|| {
            hang_tick!();
            let mut block = vec![0.0; 1024];
            let SessionStream::Offline(stream) = state.stream.as_mut().expect("stream") else {
                panic!("offline fixture")
            };
            stream
                .render(clock, 512, &mut block)
                .expect("offline render");
            hang_tick!();
            block
        })
    }

    pub(crate) fn probe_owner(
        start: impl FnMut(&mut FirewheelContext, u32) -> Result<SessionStream, String> + Send + 'static,
    ) -> (
        HostCore<TestPools>,
        kithara_platform::sync::mpsc::Receiver<DeckMsg>,
    ) {
        let state = session_state(start);
        let (sender, messages) = kithara_platform::sync::mpsc::channel();
        let inbox = Arc::new(Inbox(kithara_platform::sync::Mutex::new(sender)));
        (HostCore::new(state, inbox), messages)
    }

    pub(crate) fn mix_registration(id: BeatGridId, level: f32) -> Command {
        let prep = ResourcePrep::builder()
            .worker(PlayWorker::new(PlayWorkerConfig::builder(pools()).build()))
            .block_on_underrun(true)
            .build();
        let mix = kithara_play::DeckMixSettings::builder()
            .level(level)
            .build();
        let mixer = kithara_play::DeckMixerConfig::builder().mix(mix).build();
        let store = kithara_assets::AssetStore::builder(pools())
            .backend(kithara_assets::StorageBackend::Memory)
            .build();
        HostCommand::Register {
            id,
            deck: Box::new(Queue::new(
                QueueConfig::builder()
                    .prep(prep)
                    .store(store)
                    .mixer(mixer)
                    .build(),
            )),
        }
    }

    pub(crate) fn fixture_command(
        state: &mut TestState,
        id: BeatGridId,
        command: kithara_queue::QueueCommand<TestPools>,
    ) {
        let mut command = Some(command);
        state.owner.begin_pass();
        state
            .owner
            .with_deck(id, &mut |deck, out, pass| {
                deck.tick(pass, out);
                kithara_play::Player::apply(deck, command.take().expect("one command"), out)
                    .expect("fixture queue command");
            })
            .expect("fixture deck is held");
        state.owner.pass();
    }

    pub(crate) fn deck_snapshot<D: HostedDeck<TestPools>>(
        state: &mut TestState<D>,
        id: BeatGridId,
    ) -> kithara_render::bridge::DeckSnapshot {
        let mut snapshot = None;
        state
            .owner
            .with_deck(id, &mut |_, _, pass| snapshot = Some(pass.deck.clone()))
            .expect("fixture deck is held");
        snapshot.expect("deck was visited")
    }

    pub(crate) fn occupied_slots(state: &mut TestState, id: BeatGridId) -> Vec<usize> {
        deck_snapshot(state, id)
            .slots
            .iter()
            .enumerate()
            .filter(|(_, slot)| slot.state != kithara_render::bridge::SlotState::Empty)
            .map(|(index, _)| index)
            .collect()
    }

    #[kithara::hang_watchdog(timeout = Duration::from_secs(1))]
    pub(crate) fn fixture_pass<D: HostedDeck<TestPools>>(
        state: &mut TestState<D>,
        clock: &mut u64,
    ) -> Vec<f32> {
        kithara_platform::flash::participate_sync(|| {
            hang_tick!();
            state.owner.begin_pass();
            for message in state.messages.try_iter() {
                hang_tick!();
                message.run(&mut state.owner);
                hang_tick!();
                hang_reset!();
            }
            state
                .owner
                .each_deck(&mut |_, deck, out, pass| deck.tick(pass, out));
            state.owner.pass();
            let block = render_block(state, *clock);
            *clock += 512;
            state.owner.begin_pass();
            state.owner.pass();
            hang_tick!();
            block
        })
    }

    #[kithara::hang_watchdog(timeout = Duration::from_secs(1))]
    pub(crate) async fn play_fixture(
        state: &mut TestState,
        id: BeatGridId,
        path: &std::path::Path,
        clock: &mut u64,
        expected_slots: usize,
    ) {
        fixture_pass(state, clock);
        let item = kithara_events::TrackId::allocate();
        let config =
            kithara_play::ResourceConfig::for_src(kithara_play::ResourceSrc::Path(path.to_owned()))
                .store(
                    kithara_assets::AssetStore::builder(pools())
                        .backend(kithara_assets::StorageBackend::Memory)
                        .build(),
                )
                .build();
        fixture_command(
            state,
            id,
            kithara_queue::QueueCommand::Append {
                position: kithara_play::Position::ZERO,
                id: item,
                source: kithara_queue::TrackSource::Config(Box::new(config)),
            },
        );
        let transition = if expected_slots == 1 {
            kithara_queue::Transition::None
        } else {
            kithara_queue::Transition::Crossfade
        };
        fixture_command(
            state,
            id,
            kithara_queue::QueueCommand::Select {
                id: item,
                transition,
            },
        );
        fixture_command(
            state,
            id,
            kithara_queue::QueueCommand::Play { at: When::Next },
        );

        let deadline = kithara_platform::time::WallInstant::now() + Duration::from_secs(2);
        let mut previous = 0;
        loop {
            hang_tick!();
            fixture_pass(state, clock);
            let occupied = occupied_slots(state, id).len();
            let reached = occupied == expected_slots
                && deck_snapshot(state, id)
                    .slots
                    .iter()
                    .any(|slot| slot.state == kithara_render::bridge::SlotState::Playing);
            hang_tick!();
            assert!(
                kithara_platform::time::WallInstant::now() < deadline,
                "fixture tracks did not reach their mixer slots",
            );
            if reached {
                return;
            }
            if occupied > previous {
                hang_reset!();
                previous = occupied;
            }
            kithara_platform::tokio::task::yield_now().await;
        }
    }

    pub(crate) fn constant_fixture() -> kithara_test_utils::TestTempDir {
        let directory = kithara_test_utils::TestTempDir::new();
        let rate = NonZeroU32::new(44_100).expect("fixture rate");
        kithara_play::mock::write_pcm_wav(
            &directory.path().join("constant.wav"),
            &vec![0.5; 60 * 44_100 * 2],
            kithara_signal::AudioSpec::new(2, rate),
        )
        .expect("constant PCM fixture");
        directory
    }
}
