use arc_swap::ArcSwap;
use kithara::{
    host::{DeckId, HostSettingsControl, api::Tempo},
    platform::{
        sync::Arc,
        time::Duration,
        tokio::{
            sync::mpsc::{self, UnboundedReceiver, error::TryRecvError},
            task,
        },
    },
};
use kithara_app_library::Playable;
use tracing::{error, info};

use super::{
    command::{AppCmd, Command, DeckCmd, Envelope, MixCmd},
    snapshot::{BroadcastPhase, EngineSnapshot},
};
use crate::{
    analysis::AnalysisHandle,
    broadcast::Broadcaster,
    catalog,
    config::AppConfig,
    deck::{DeckRecord, DeckSet, EqMode},
};

#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in)]
pub(crate) struct Engine {
    config: AppConfig,
    analysis: AnalysisHandle,
    snapshots: Arc<ArcSwap<EngineSnapshot>>,
    broadcast: Broadcaster,
    session: DeckSet,
    eq_mode: EqMode,
    #[field(get = is_shut_down, vis = "pub(super)", copy)]
    has_shut_down: bool,
    drain: Option<UnboundedReceiver<Option<Duration>>>,
    applied_seq: u64,
    accepted_tempo: Tempo,
}

impl Engine {
    pub(crate) fn new(
        session: DeckSet,
        config: AppConfig,
        broadcast: Broadcaster,
        snapshots: Arc<ArcSwap<EngineSnapshot>>,
        analysis: AnalysisHandle,
    ) -> Self {
        let accepted_tempo = session.host().tempo();
        let engine = Self {
            accepted_tempo,
            broadcast,
            analysis,
            config,
            session,
            snapshots,
            eq_mode: EqMode::default(),
            has_shut_down: false,
            drain: None,
            applied_seq: 0,
        };

        engine.publish();
        engine
    }

    pub(crate) fn apply(&mut self, envelope: Envelope) {
        match envelope.command {
            Command::Deck { deck, cmd } => self.apply_deck(deck, cmd),
            Command::Mix(cmd) => self.apply_mix(cmd),
            Command::LoadOntoDeck { deck, track } => self.load(deck, &track),
            Command::App(cmd) => self.apply_app(cmd),
        }
        self.applied_seq = envelope.seq;
    }

    fn apply_app(&mut self, cmd: AppCmd) {
        match cmd {
            AppCmd::SetEqMode(mode) => self.set_eq_mode(mode),
            AppCmd::SetTempo(tempo) => self.set_tempo(tempo),
            AppCmd::StepTempo(steps) => self.step_tempo(steps),
            AppCmd::BroadcastToggle => self.toggle_broadcast(),
            AppCmd::Shutdown => self.shut_down(),
        }
    }

    fn apply_deck(&mut self, id: DeckId, cmd: DeckCmd) {
        self.session.apply(id, cmd, self.eq_mode);
    }

    fn apply_mix(&mut self, cmd: MixCmd) {
        let result = match cmd {
            MixCmd::Crossfader(position) => self.session.set_crossfader(position),
            MixCmd::Trim(id, trim) => self.session.set_trim(id, trim),
        };
        if let Err(error) = result {
            error!(?cmd, %error, "mix edit rejected");
        }
    }

    pub(super) fn cadence(&self) -> Duration {
        const ACTIVE: Duration = Duration::from_millis(16);
        const IDLE: Duration = Duration::from_millis(500);
        let playing = self.snapshots.load().decks.iter().any(|deck| deck.playing);
        if playing || self.broadcast.is_pending() {
            ACTIVE
        } else {
            IDLE
        }
    }

    fn complete_stop(&mut self, elapsed: Option<Duration>) {
        self.broadcast.complete_stop();
        if let Some(elapsed) = elapsed {
            info!(
                elapsed_ms = elapsed.as_secs_f64() * 1_000.0,
                "broadcast stopped"
            );
        }
    }

    fn finish_drain(&mut self) {
        let Some(drain) = self.drain.as_mut() else {
            return;
        };
        let elapsed = match drain.try_recv() {
            Err(TryRecvError::Empty) => return,
            Ok(elapsed) => elapsed,
            Err(TryRecvError::Disconnected) => None,
        };
        self.drain = None;
        self.complete_stop(elapsed);
    }

    fn load(&self, id: DeckId, track: &Playable) {
        let Some(record) = self.session.record(id) else {
            return;
        };

        if let Err(e) = catalog::load_onto(record.queue(), track, &self.config) {
            error!(source = %track.source, deck = %id, error = %e, "load onto deck failed");
        }
    }

    pub(crate) fn publish(&self) {
        self.snapshots.store(Arc::new(self.snapshot()));
    }

    fn set_eq_mode(&mut self, mode: EqMode) {
        if self.eq_mode != mode && self.session.set_eq_mode(self.eq_mode, mode) {
            self.eq_mode = mode;
        }
    }

    fn set_tempo(&mut self, tempo: Tempo) {
        match self.session.host().set_tempo(tempo) {
            Ok(()) => self.accepted_tempo = tempo,
            Err(error) => error!(?tempo, %error, "host tempo refused"),
        }
    }

    fn step_tempo(&mut self, steps: f32) {
        const STEP_BPM: f64 = 0.1;

        let bpm = f64::from(steps).mul_add(STEP_BPM, f64::from(self.accepted_tempo));
        if let Ok(tempo) = Tempo::new(bpm) {
            self.set_tempo(tempo);
        }
    }

    fn shut_down(&mut self) {
        if let Some(stop) = self.broadcast.shut_down(self.session.host()) {
            let elapsed = stop.drain();
            self.complete_stop(Some(elapsed));
        }

        self.session.close();
        self.has_shut_down = true;
    }

    fn snapshot(&self) -> EngineSnapshot {
        EngineSnapshot {
            track_bpms: Arc::clone(&self.analysis.bpms()),
            decks: self
                .session
                .records()
                .iter()
                .map(DeckRecord::snapshot)
                .collect(),
            broadcast: BroadcastPhase::new(&self.broadcast),
            eq_mode: self.eq_mode,
            mix: *self.session.mix(),
            tempo: self.session.host().tempo(),
            applied_seq: self.applied_seq,
        }
    }

    pub(crate) fn tick(&mut self) {
        self.broadcast.poll(self.session.host());
        self.finish_drain();

        for record in self.session.records() {
            record.refresh();
        }
    }

    fn toggle_broadcast(&mut self) {
        let Some(stop) = self.broadcast.toggle(self.session.host()) else {
            return;
        };
        let (done, drain) = mpsc::unbounded_channel();
        task::spawn(async move {
            let _ = done.send(stop.run().await);
        });
        self.drain = Some(drain);
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.broadcast.release(self.session.host());
    }
}

#[cfg(all(test, feature = "mock", feature = "gui"))]
pub(crate) mod mock {
    use crate::gui::rig::Rig;

    pub(crate) fn host(rig: &mut Rig) -> &mut crate::pools::AppHost {
        crate::deck::mock::host(&mut rig.engine.session)
    }

    pub(crate) fn render(rig: &mut Rig, frames: u64) {
        let host = crate::deck::mock::host(&mut rig.engine.session);
        let start = rig.rendered_frames;
        let end = start.checked_add(frames).expect("fixture frame range");
        let report = crate::mock::render_host(host, start..end, &rig.shutdown)
            .expect("the offline app output renders");
        assert_eq!(report.frames, frames);
        rig.rendered_frames = end;
    }
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;

    use ::kithara::{
        platform::{CancelToken, time::Instant},
        queue::QueueControl,
    };
    use kithara_test_utils::{kithara, off_thread::OffThread};

    #[cfg(not(feature = "broadcast"))]
    use crate::engine::{Command, DeckCmd, Envelope};
    use crate::{analysis::fixtures::tone_mp3, gui::rig::Rig};

    fn close(rig: &mut Rig) {
        rig.close_window();
        let started = Instant::now();
        let applied = rig.pump();
        assert!(
            started.elapsed() < Rig::DEADLINE,
            "the teardown finishes within {:?}",
            Rig::DEADLINE
        );
        assert_eq!(applied.len(), 1, "closing the window queues one shutdown");
        assert!(
            rig.queues.iter().all(QueueControl::is_closed),
            "every deck left the host"
        );
        assert!(
            rig.deck_tokens.iter().all(CancelToken::is_cancelled),
            "the deck tasks are cancelled"
        );
        assert!(
            !rig.shutdown.is_cancelled(),
            "the app root is cancelled by the frontend after the engine reports"
        );
    }

    #[cfg(not(feature = "broadcast"))]
    #[kithara::test(native, tokio, flash(false))]
    async fn the_shutdown_command_takes_the_decks_off_the_host_and_cancels_their_tasks() {
        let rig = OffThread::spawn("engine", || Ok::<_, Infallible>(Rig::offline()))
            .await
            .expect("rig fixture is infallible");
        rig.call(close).await;
        rig.close().await;
    }

    #[cfg(feature = "broadcast")]
    #[kithara::test(native, tokio, flash(false))]
    async fn closing_the_window_on_air_drains_the_broadcast_and_takes_the_decks_off(
        tone_mp3: String,
    ) {
        use ::kithara::ui::render::ControlAction;

        let rig = OffThread::spawn("engine", || Ok::<_, Infallible>(Rig::on_air()))
            .await
            .expect("rig fixture is infallible");
        rig.call(move |rig| {
            rig.queues[0]
                .append(tone_mp3.as_str())
                .expect("deck A takes the track");
            rig.send("deck-a/play", ControlAction::Activate);
            rig.send("bar/broadcast", ControlAction::Activate);
            let applied = rig.pump();
            assert_eq!(applied.len(), 2, "play and the air toggle");
            assert_eq!(rig.applied_seq(), applied[1]);
            rig.until(
                "the broadcast goes on air",
                Rig::DEADLINE,
                |rig| {
                    rig.engine.tick();
                    rig.engine.publish();
                    rig.frame();
                },
                |rig| rig.flag("broadcast.on_air"),
            );

            close(rig);
            rig.frame();
            assert!(!rig.flag("broadcast.on_air"), "the broadcast is released");
        })
        .await;
        rig.close().await;
    }

    #[cfg(not(feature = "broadcast"))]
    #[kithara::test(native, tokio, flash(false))]
    async fn a_seek_lands_on_the_duration_the_queue_knows_before_the_next_tick(tone_mp3: String) {
        let rig = OffThread::spawn("engine", || Ok::<_, Infallible>(Rig::offline()))
            .await
            .expect("rig fixture is infallible");

        rig.call(move |rig| {
            let id = rig.queues[0]
                .append(tone_mp3.as_str())
                .expect("deck A takes the track");
            rig.queues[0]
                .select(id, ::kithara::queue::Transition::None)
                .expect("deck A loads the selected track while paused");

            let queue_tick = |rig: &mut Rig| {
                let _ = rig.queues[0].tick();
                crate::mock::render_rig(rig, 512);
            };

            rig.until(
                "the queue learns the duration",
                Rig::DEADLINE,
                queue_tick,
                |rig| rig.queues[0].duration_seconds().is_some(),
            );

            let duration = rig.queues[0]
                .duration_seconds()
                .expect("the queue knows the duration");

            let deck = rig.deck("a");
            rig.engine.apply(Envelope {
                command: Command::Deck {
                    deck,
                    cmd: DeckCmd::SeekFraction(0.9),
                },
                seq: 1,
            });

            rig.until(
                "the seek lands near the end of the track",
                Rig::DEADLINE,
                queue_tick,
                |rig| {
                    rig.queues[0]
                        .position_seconds()
                        .is_some_and(|position| position >= duration * 0.8)
                },
            );
        })
        .await;

        rig.close().await;
    }

    #[cfg(not(feature = "broadcast"))]
    #[kithara::test(native, flash(false))]
    fn a_rung_picked_on_a_deck_without_a_ladder_publishes_the_ladder_mode() {
        let mut rig = Rig::offline();
        let deck = rig.deck("a");

        rig.engine.apply(Envelope {
            command: Command::Deck {
                deck,
                cmd: DeckCmd::SetQuality(Some(1)),
            },
            seq: 1,
        });
        rig.engine.publish();

        let published = rig.snapshots.load();
        let stream = &published.deck(deck).expect("deck A").stream;
        assert_eq!(
            (stream.selected, stream.is_auto),
            (None, true),
            "a deck with no ladder shows no pinned rung"
        );
    }
}
