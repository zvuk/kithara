use std::num::NonZeroU32;

use kithara_play::CrossfadeSettings;

use super::*;
use crate::{QueueSettings, QueueSettingsChange, Transition};

const SAMPLE_RATE: u32 = 48_000;
const BLOCK_FRAMES: usize = 128;
const FADE_SECONDS: f32 = 0.25;
const WARMUP_BLOCKS: usize = 24;
const SETTLE_BLOCKS: usize = 40;
const MAX_STEP: f32 = 0.01;
const EXACT: f32 = 1.0e-6;

struct DurationRig {
    queue: Queue<TestPools>,
    mixer: mock::MixerRig,
    loads: mock::DeckRig<TestPools>,
    dir: TestTempDir,
    frame: i64,
}

impl DurationRig {
    fn new() -> Self {
        let rate = NonZeroU32::new(SAMPLE_RATE).expect("nonzero sample rate");
        let config = DeckMixerConfig::builder().sample_rate(rate).build();
        let shape = kithara_render::rt::StreamShape::new(
            NonZeroU32::new(128).expect("nonzero block size"),
            rate,
        );
        let prep = ResourcePrep::builder()
            .worker(PlayWorker::new(PlayWorkerConfig::builder(pools()).build()))
            .block_on_underrun(true)
            .build();
        let settings = QueueSettings::builder()
            .crossfade(CrossfadeSettings {
                duration: FADE_SECONDS,
                ..CrossfadeSettings::default()
            })
            .build();
        let store = AssetStore::builder(pools())
            .backend(StorageBackend::Memory)
            .build();
        Self {
            queue: Queue::new(
                QueueConfig::builder()
                    .prep(prep)
                    .store(store)
                    .mixer(config)
                    .settings(settings)
                    .build(),
            ),
            mixer: mock::MixerRig::new(config, shape, &pools()).expect("real mixer"),
            loads: mock::DeckRig::new(config).expect("load dispatcher"),
            dir: TestTempDir::new(),
            frame: 0,
        }
    }

    fn in_pass<Value>(
        &mut self,
        run: impl FnOnce(&mut Queue<TestPools>, &mut Outbox<'_, TestPools>) -> Value,
    ) -> Value {
        let rate = NonZeroU32::new(SAMPLE_RATE).expect("nonzero sample rate");
        let shape =
            kithara_render::rt::StreamShape::new(NonZeroU32::new(128).expect("block"), rate);
        let mut output = mock::output(Some(shape)).get();
        output.sample_rate = kithara_play::SessionSampleRate::new(Some(SAMPLE_RATE), SAMPLE_RATE);
        let deck = self.mixer.ends.snapshot.read().clone();
        let pass = DeckPass {
            mix: self.queue.deck.mix,
            suspended: false,
            now: SessionFrame::new(self.frame),
            delivery: FrameCount::new(BLOCK_FRAMES),
            output: &output,
            deck: &deck,
        };
        let mut scope = self
            .mixer
            .ring
            .scope(self.mixer.scope)
            .expect("real deck scope");
        let mut out = Outbox::new(&mut scope, &mut self.loads.dispatcher).in_pass(pass);
        HostedDeck::tick(&mut self.queue, pass, &mut out);
        run(&mut self.queue, &mut out)
    }

    fn apply(&mut self, command: QueueCommand<TestPools>) {
        self.in_pass(|queue, out| Player::apply(queue, command, out))
            .expect("queue command");
    }

    #[kithara::hang_watchdog(timeout = Duration::from_secs(1))]
    async fn select(&mut self, level: f32, name: &str) {
        use kithara_render::{Dispatched, DispatcherCommand};

        let path = self.dir.path().join(name);
        mock::write_pcm_wav(
            &path,
            &vec![level; 60 * 48_000 * 2],
            AudioSpec::new(2, NonZeroU32::new(SAMPLE_RATE).expect("rate")),
        )
        .expect("constant WAV");
        let id = TrackId::allocate();
        self.apply(QueueCommand::Append {
            position: kithara_play::Position::ZERO,
            id,
            source: TrackSource::Uri(path.to_str().expect("fixture path").to_owned()),
        });
        self.apply(QueueCommand::Select {
            id,
            transition: Transition::Crossfade,
        });
        self.apply(QueueCommand::Play { at: When::Next });
        let wanted = self
            .queue
            .active
            .iter()
            .find(|active| active.item == id)
            .and_then(|active| active.load)
            .expect("the selected fixture is loading")
            .seq();
        self.loads.opens.drain();
        let (request, seq) = loop {
            hang_tick!();
            let mut due = self
                .loads
                .opens
                .next_due((), 1)
                .expect("pending queue load");
            let seq = due.seq();
            let command = due.commands_mut().pop().expect("queue dispatcher command");
            assert!(due.commands_mut().is_empty());
            match command {
                DispatcherCommand::Load(request) => {
                    assert_eq!(seq, wanted, "the fixture opens its selected load");
                    break (request, due.defer());
                }
                DispatcherCommand::SetPriority(_, _) => due.apply(Dispatched::Prioritized),
                DispatcherCommand::Release(_) => due.apply(Dispatched::Released),
            }
            let receipt = self
                .loads
                .dispatcher
                .receipts()
                .next()
                .expect("dispatcher maintenance receipt");
            assert_eq!(receipt.seq(), seq);
            self.in_pass(|queue, out| Player::settle(queue, TrackReceipt::Loaded(receipt), out));
            hang_reset!();
        };
        let loaded = {
            let mut opening = std::pin::pin!(
                self.loads
                    .load_request_fixture(self.queue.worker().expect("fixture worker"), *request)
            );
            loop {
                hang_tick!();
                if let Ok(loaded) =
                    kithara_platform::time::timeout(Duration::from_millis(10), opening.as_mut())
                        .await
                {
                    hang_reset!();
                    break loaded.expect("fixture load");
                }
            }
        };
        self.loads
            .opens
            .resume(seq, (), ())
            .expect("queue open resumes")
            .apply(Dispatched::Loaded(loaded));
        let receipt = self
            .loads
            .dispatcher
            .receipts()
            .next()
            .expect("load receipt");
        self.in_pass(|queue, out| Player::settle(queue, TrackReceipt::Loaded(receipt), out));
    }

    #[kithara::hang_watchdog(timeout = Duration::from_secs(1))]
    async fn pump(mut self, blocks: usize) -> (Self, Vec<f32>) {
        kithara_platform::tokio::task::spawn_blocking(move || {
            let mut rendered = Vec::with_capacity(blocks * BLOCK_FRAMES);
            for _ in 0..blocks {
                hang_tick!();
                let mut pcm = [vec![0.0; BLOCK_FRAMES], vec![0.0; BLOCK_FRAMES]];
                let [left, right] = &mut pcm;
                self.mixer
                    .block_frames(SessionFrame::new(self.frame), [left, right])
                    .expect("real mixer block");
                while let Some(ScopedReceipt::Scope(_, receipt)) = self.mixer.ring.receipt() {
                    hang_tick!();
                    let seq = receipt.seq();
                    let (outcome, mut batch) = receipt.into();
                    self.in_pass(|queue, out| {
                        Player::settle(
                            queue,
                            TrackReceipt::Deck {
                                seq,
                                outcome: &outcome,
                                batch: &mut batch,
                            },
                            out,
                        )
                    });
                    hang_reset!();
                }
                let events = self.mixer.ends.events.drain().collect::<Vec<_>>();
                for event in events {
                    hang_tick!();
                    self.in_pass(|queue, out| {
                        Player::settle(queue, TrackReceipt::Event(event), out)
                    });
                    hang_reset!();
                }
                self.frame += 128;
                rendered.extend_from_slice(&pcm[0]);
                hang_reset!();
            }
            (self, rendered)
        })
        .await
        .expect("duration render must join")
    }

    fn configure_duration(&mut self, duration: f32) {
        let settings = CrossfadeSettings {
            duration,
            ..self.queue.control().crossfade_settings()
        };
        self.apply(QueueCommand::ConfigureQueue(
            QueueSettingsChange::Crossfade(settings),
            When::Next,
        ));
    }
}

fn last(rendered: &[f32]) -> f32 {
    rendered.last().copied().expect("blocks were rendered")
}

fn max_step_across(before: &[f32], after: &[f32]) -> f32 {
    let mut samples = vec![last(before)];
    samples.extend_from_slice(after);
    samples
        .windows(2)
        .fold(0.0_f32, |worst, pair| worst.max((pair[1] - pair[0]).abs()))
}

async fn fading_in() -> (DurationRig, Vec<f32>) {
    let mut rig = DurationRig::new();
    rig.select(0.5, "first.wav").await;
    let (rig, fading) = rig.pump(WARMUP_BLOCKS).await;
    let level = last(&fading);
    assert!(
        level > 0.0 && level < 0.5 * 0.9,
        "the fade-in is still climbing ({level})"
    );
    (rig, fading)
}

#[kithara::test(tokio)]
async fn resending_the_crossfade_duration_does_not_snap_the_mix() {
    let (mut rig, fading) = fading_in().await;
    rig.configure_duration(FADE_SECONDS);
    let (_, resent) = rig.pump(WARMUP_BLOCKS).await;
    let step = max_step_across(&fading, &resent);
    assert!(
        step <= MAX_STEP,
        "an unchanged crossfade duration must leave the fade alone (step {step})"
    );
}

#[kithara::test(tokio)]
async fn changing_the_crossfade_duration_mid_fade_keeps_the_running_fade() {
    let (mut rig, fading) = fading_in().await;
    rig.configure_duration(FADE_SECONDS / 10.0);
    let (_, changed) = rig.pump(WARMUP_BLOCKS).await;
    let step = max_step_across(&fading, &changed);
    assert!(
        step <= MAX_STEP,
        "a changed crossfade duration must leave the running fade alone (step {step})"
    );
    let level = last(&changed);
    assert!(
        level < 0.5 * 0.9,
        "the running fade keeps its original duration: it is still climbing after the change ({level})"
    );
}

#[kithara::test(tokio)]
async fn a_changed_crossfade_duration_applies_to_the_next_fade() {
    let (rig, _) = fading_in().await;
    let (mut rig, settled) = rig.pump(SETTLE_BLOCKS * 20).await;
    assert!(
        (last(&settled) - 0.5).abs() < EXACT,
        "the first fade has settled under its original duration before the change ({})",
        last(&settled)
    );
    rig.configure_duration(FADE_SECONDS / 10.0);
    rig.select(0.25, "second.wav").await;
    let (_, handed_over) = rig.pump(SETTLE_BLOCKS * 3).await;
    assert!(
        (last(&handed_over) - 0.25).abs() < EXACT,
        "the next fade runs under the new duration: a tenth of the original settles within three settle windows, the original would not ({})",
        last(&handed_over)
    );
}
