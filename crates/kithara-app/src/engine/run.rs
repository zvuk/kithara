use kithara::platform::{
    CancelToken,
    time::{self, Instant},
    tokio::{self, sync::mpsc::UnboundedReceiver},
};

use super::{Engine, Envelope};

pub(crate) async fn run(
    mut engine: Engine,
    mut commands: UnboundedReceiver<Envelope>,
    cancel: CancelToken,
) -> bool {
    let mut next_tick = Instant::now() + engine.cadence();
    loop {
        tokio::select! {
            biased;
            () = cancel.cancelled() => break,
            () = time::sleep(next_tick.saturating_duration_since(Instant::now())) => {
                engine.tick();
                engine.publish();
                next_tick = Instant::now() + engine.cadence();
            }
            received = commands.recv() => {
                let Some(envelope) = received else { break };
                engine.apply(envelope);
                while !engine.is_shut_down()
                    && let Ok(envelope) = commands.try_recv()
                {
                    engine.apply(envelope);
                }
                engine.publish();
                if engine.is_shut_down() {
                    break;
                }
                next_tick = next_tick.min(Instant::now() + engine.cadence());
            }
        }
    }
    engine.is_shut_down()
}

#[cfg(all(test, not(feature = "broadcast")))]
mod tests {
    use std::convert::Infallible;

    use ::kithara::{
        platform::{
            time,
            tokio::{runtime::Handle, sync::oneshot},
        },
        ui::render::ControlAction,
    };
    use kithara_test_utils::{kithara, off_thread::OffThread};

    use crate::{
        analysis::fixtures::{short_wav, tone_mp3},
        deck::DeckId,
        engine::{Command, DeckCmd, Envelope, MixCmd, serve},
        gui::rig::Rig,
    };

    #[kithara::test(native, tokio, flash(false))]
    async fn the_ui_draws_and_queues_while_the_engine_drains_nothing() {
        let rig = OffThread::spawn("engine", || Ok::<_, Infallible>(Rig::offline()))
            .await
            .expect("rig fixture is infallible");
        rig.call(|rig| {
            let echoed = rig.applied_seq();

            rig.send("mixer/xfade", ControlAction::SetScalar(1.0));
            rig.send("deck-a/play", ControlAction::Activate);
            rig.send("mixer/a/mute", ControlAction::Activate);
            for _ in 0..3 {
                rig.frame();
            }

            assert!((rig.scalar("mix.crossfader") - 1.0).abs() < f64::EPSILON);
            assert!(rig.flag("mixer.muted@deck=a"));
            assert!(
                !rig.flag("deck.playback.playing@deck=a"),
                "whether the deck plays is the engine's to report"
            );
            assert_eq!(rig.applied_seq(), echoed, "the engine applied nothing");
            assert!((rig.snapshots.load().mix.position - 0.5).abs() < f32::EPSILON);

            let queued: Vec<Envelope> =
                std::iter::from_fn(|| rig.commands.try_recv().ok()).collect();
            assert!(queued.first().is_some_and(|first| first.seq > echoed));
            assert!(queued.windows(2).all(|pair| pair[0].seq < pair[1].seq));
            assert!(
                matches!(
                    queued.as_slice(),
                    [
                        Envelope {
                            command: Command::Mix(MixCmd::Crossfader(position)),
                            ..
                        },
                        Envelope {
                            command: Command::Deck {
                                deck: DeckId(0),
                                cmd: DeckCmd::Play,
                            },
                            ..
                        },
                        Envelope {
                            command: Command::Mix(MixCmd::Muted(DeckId(0), true)),
                            ..
                        },
                    ] if (*position - 1.0).abs() < f32::EPSILON
                ),
                "every message queued its intent, in order"
            );
        })
        .await;
        rig.close().await;
    }

    #[kithara::test(native, tokio, flash(false))]
    async fn playback_advances_on_the_engine_tick_alone(tone_mp3: String, short_wav: String) {
        let rig = OffThread::spawn("engine", || Ok::<_, Infallible>(Rig::realtime()))
            .await
            .expect("rig fixture is infallible");
        rig.call(move |rig| {
            let queue = rig.queues[0].clone();
            queue
                .append(tone_mp3.as_str())
                .expect("deck A takes the track");
            queue
                .append(short_wav.as_str())
                .expect("deck A takes the next track");
            let next = queue.tracks()[1].name.clone();
            let engine_tick = |rig: &mut Rig| {
                rig.engine.tick();
                rig.engine.publish();
            };

            rig.send("deck-a/play", ControlAction::Activate);
            rig.pump();
            rig.until("the first track plays", Rig::DEADLINE, engine_tick, |rig| {
                rig.queues[0].is_playing()
                    && rig.queues[0]
                        .duration_seconds()
                        .is_some_and(|seconds| seconds > 0.0)
            });
            rig.send("deck-a/wave", ControlAction::SetScalar(0.9));
            rig.pump();
            rig.until(
                "the queue advances with no UI frame",
                Rig::DEADLINE,
                engine_tick,
                |rig| rig.queues[0].current_index() == Some(1),
            );

            rig.until(
                "the next snapshot names the next track",
                Rig::DEADLINE,
                |rig| {
                    engine_tick(rig);
                    rig.frame();
                },
                |rig| rig.text("deck.track.title@deck=a").as_deref() == Some(next.as_str()),
            );
        })
        .await;
        rig.close().await;
    }

    #[kithara::test(native, tokio, flash(false))]
    async fn the_loop_ends_on_the_shutdown_command_while_the_ui_holds_its_sender() {
        let rig = OffThread::spawn("engine", || Ok::<_, Infallible>(Some(Rig::offline())))
            .await
            .expect("rig fixture is infallible");
        rig.call(|slot| {
            let mut rig = slot.take().expect("the rig is set up once");
            rig.close_window();
            let Rig {
                engine,
                ui,
                commands,
                shutdown,
                ..
            } = rig;
            let shut_down = Handle::current()
                .block_on(time::timeout(
                    Rig::DEADLINE,
                    crate::engine::run(engine, commands, shutdown.child()),
                ))
                .expect("the loop returns on the shutdown command");
            assert!(shut_down, "the loop reports that the shutdown ran");
            drop(ui);
        })
        .await;
        rig.close().await;
    }

    #[kithara::test(native, tokio, flash(false))]
    async fn a_built_engine_runs_no_loop_once_the_root_stopped_waiting() {
        let rig = OffThread::spawn("engine", || Ok::<_, Infallible>(Some(Rig::offline())))
            .await
            .expect("rig fixture is infallible");
        rig.call(|slot| {
            let Rig {
                engine,
                ui,
                commands,
                shutdown,
                ..
            } = slot.take().expect("the rig is set up once");
            let (built_tx, built) = oneshot::channel();
            drop(built);
            Handle::current()
                .block_on(time::timeout(
                    Rig::DEADLINE,
                    serve(move || Ok(engine), built_tx, commands, shutdown.child()),
                ))
                .expect("the loop would run while the UI holds its sender");
            drop(ui);
        })
        .await;
        rig.close().await;
    }
}
