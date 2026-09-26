use arc_swap::ArcSwap;
use kithara::{
    abr::{AbrHandle, AbrMode},
    effects::{GainDb, eq::EqBandConfig},
    platform::{
        sync::Arc,
        time::Duration,
        tokio::{
            sync::mpsc::{self, UnboundedReceiver, error::TryRecvError},
            task,
        },
    },
    queue::Transition,
};
use tracing::{debug, error, info};

use super::{
    command::{AppCmd, Command, DeckCmd, Envelope, MixCmd},
    settings::DeckSettings,
    snapshot::{BroadcastPhase, DeckSnapshot, EngineSnapshot},
};
use crate::{
    broadcast::Broadcaster,
    catalog,
    config::AppConfig,
    deck::{Deck, DeckId, DeckSet, EqMode},
    state::StateController,
};

#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in)]
pub(crate) struct Engine {
    config: AppConfig,
    snapshots: Arc<ArcSwap<EngineSnapshot>>,
    broadcast: Broadcaster,
    session: DeckSet,
    eq_mode: EqMode,
    #[field(get = is_shut_down, vis = "pub(super)", copy)]
    has_shut_down: bool,
    drain: Option<UnboundedReceiver<Option<Duration>>>,
    decks: Vec<EngineDeck>,
    applied_seq: u64,
}

struct EngineDeck {
    controller: StateController,
    settings: DeckSettings,
}

struct EqModeChange<'a> {
    controller: &'a StateController,
    id: DeckId,
    gains: Vec<GainDb>,
    next: Vec<EqBandConfig>,
    previous: Vec<EqBandConfig>,
}

impl Engine {
    pub(crate) fn new(
        session: DeckSet,
        config: AppConfig,
        broadcast: Broadcaster,
        snapshots: Arc<ArcSwap<EngineSnapshot>>,
        mut controller: impl FnMut(&Deck) -> StateController,
    ) -> Self {
        let decks = session
            .decks()
            .iter()
            .map(|deck| {
                let controller = controller(deck);
                let settings = DeckSettings::new(controller.queue().eq_band_count());
                EngineDeck {
                    controller,
                    settings,
                }
            })
            .collect();
        let engine = Self {
            broadcast,
            config,
            session,
            decks,
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
            Command::LoadOntoDeck { deck, source } => self.load(deck, &source),
            Command::App(cmd) => self.apply_app(cmd),
        }
        self.applied_seq = envelope.seq;
    }

    fn apply_app(&mut self, cmd: AppCmd) {
        match cmd {
            AppCmd::SetEqMode(mode) => self.set_eq_mode(mode),
            AppCmd::BroadcastToggle => self.toggle_broadcast(),
            AppCmd::Shutdown => self.shut_down(),
        }
    }

    fn apply_deck(&mut self, id: DeckId, cmd: DeckCmd) {
        let eq_mode = self.eq_mode;
        if let Some(deck) = self.deck_mut(id) {
            deck.apply(cmd, eq_mode);
        }
    }

    fn apply_mix(&mut self, cmd: MixCmd) {
        let result = match cmd {
            MixCmd::Crossfader(position) => self.session.set_crossfader(position),
            MixCmd::Master(gain) => self.session.set_group_master(gain),
            MixCmd::Muted(id, muted) => self.session.set_muted(id, muted),
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

    fn deck(&self, id: DeckId) -> Option<&EngineDeck> {
        self.session.position(id).and_then(|at| self.decks.get(at))
    }

    fn deck_mut(&mut self, id: DeckId) -> Option<&mut EngineDeck> {
        self.session
            .position(id)
            .and_then(|at| self.decks.get_mut(at))
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

    fn load(&self, id: DeckId, source: &str) {
        let Some(deck) = self.deck(id) else {
            return;
        };
        if let Err(e) = catalog::load_onto(deck.controller.queue(), source, &self.config) {
            error!(source, deck = id.0, error = %e, "load onto deck failed");
        }
    }

    pub(crate) fn publish(&self) {
        self.snapshots.store(Arc::new(self.snapshot()));
    }

    fn rollback_eq_mode(changes: &[EqModeChange<'_>]) {
        for change in changes.iter().rev() {
            if let Err(err) = change
                .controller
                .queue()
                .set_eq_layout(change.previous.clone())
            {
                error!(
                    deck = change.id.0,
                    error = ?err,
                    "rollback shared EQ layout failed"
                );
            }
        }
    }

    fn set_eq_mode(&mut self, mode: EqMode) {
        let current_mode = self.eq_mode;
        if current_mode == mode {
            return;
        }

        let mut changes: Vec<EqModeChange<'_>> = Vec::new();
        for (deck, engine_deck) in self.session.decks().iter().zip(&self.decks) {
            let current = &engine_deck.settings.eq_bands;
            let Some(gains) = current_mode.remap(mode, current) else {
                error!(
                    deck = deck.id.0,
                    current = ?current_mode,
                    requested = ?mode,
                    bands = current.len(),
                    "EQ mode state does not match its band layout"
                );
                return;
            };
            changes.push(EqModeChange {
                id: deck.id,
                controller: &engine_deck.controller,
                previous: current_mode.layout(current),
                next: mode.layout(&gains),
                gains,
            });
        }

        for (applied, change) in changes.iter().enumerate() {
            if let Err(err) = change.controller.queue().set_eq_layout(change.next.clone()) {
                error!(
                    deck = change.id.0,
                    requested = ?mode,
                    error = ?err,
                    "set shared EQ layout failed"
                );
                Self::rollback_eq_mode(&changes[..applied]);
                return;
            }
        }

        let gains: Vec<Vec<GainDb>> = changes.into_iter().map(|change| change.gains).collect();
        for (deck, gains) in self.decks.iter_mut().zip(gains) {
            deck.settings.eq_bands = gains;
        }
        self.eq_mode = mode;
    }

    fn shut_down(&mut self) {
        if let Some(stop) = self.broadcast.shut_down(self.session.host()) {
            let elapsed = stop.drain();
            self.complete_stop(Some(elapsed));
        }
        self.session.close();
        self.decks.clear();
        self.has_shut_down = true;
    }

    fn snapshot(&self) -> EngineSnapshot {
        let decks = self
            .session
            .decks()
            .iter()
            .zip(&self.decks)
            .map(|(deck, engine_deck)| {
                engine_deck
                    .controller
                    .read(|state| DeckSnapshot::new(deck.id, state, &engine_deck.settings))
            })
            .collect();
        EngineSnapshot {
            decks,
            broadcast: BroadcastPhase::new(&self.broadcast),
            eq_mode: self.eq_mode,
            mix: self.session.mix().clone(),
            applied_seq: self.applied_seq,
        }
    }

    pub(crate) fn tick(&mut self) {
        self.broadcast.poll(self.session.host());
        self.finish_drain();
        for deck in &self.decks {
            let _ = deck.controller.queue().tick();
            deck.controller.refresh_continuous();
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

impl EngineDeck {
    fn apply(&mut self, cmd: DeckCmd, eq_mode: EqMode) {
        let queue = self.controller.queue();
        match cmd {
            DeckCmd::Play => queue.play(),
            DeckCmd::Pause => queue.pause(),
            DeckCmd::Next => {
                if let Err(error) = queue.next(Transition::Crossfade) {
                    error!(%error, "advance to next track failed");
                }
            }
            DeckCmd::Prev => {
                if let Err(error) = queue.previous(Transition::Crossfade) {
                    error!(%error, "return to previous track failed");
                }
            }
            DeckCmd::SeekFraction(fraction) => self.seek(fraction),
            DeckCmd::SetEqGain { layout, band, gain } if layout == eq_mode => {
                self.set_eq_gain(band, gain);
            }
            DeckCmd::SetEqGain { layout, band, .. } => {
                debug!(
                    ?layout,
                    ?eq_mode,
                    band,
                    "EQ gain for a replaced band layout rejected"
                );
            }
            DeckCmd::RemoveTrack(track) => {
                if let Err(e) = queue.remove(track) {
                    error!(?track, error = %e, "remove failed");
                }
            }
            DeckCmd::SetTempo(tempo) => {
                self.settings.tempo = tempo;
                queue.set_rate(tempo.speed());
            }
            DeckCmd::SetQuality(variant) => self.set_quality(variant),
        }
    }

    fn seek(&self, fraction: f64) {
        let duration = self.controller.read(|state| state.duration).max(0.0);
        let target = fraction.clamp(0.0, 1.0) * duration;
        if let Err(e) = self.controller.queue().seek(target) {
            error!("seek failed: {e:?}");
        }
    }

    fn set_eq_gain(&mut self, band: usize, gain: GainDb) {
        let Some(slot) = self.settings.eq_bands.get_mut(band) else {
            return;
        };
        *slot = gain;
        if let Err(e) = self.controller.queue().set_eq_gain(band, f32::from(gain)) {
            debug!(band, db = f32::from(gain), error = ?e, "set EQ gain deferred");
        }
    }

    fn set_quality(&self, variant: Option<usize>) {
        let handle = self.controller.queue().current_abr_handle();
        let requested = variant.map_or(AbrMode::Auto(None), AbrMode::manual);
        if let Some(handle) = &handle
            && let Err(error) = handle.set_mode(requested)
        {
            error!("abr mode failed: {error:?}");
        }
        let mode = handle.as_ref().and_then(AbrHandle::mode);
        self.controller.mutate(|state| state.abr_mode = mode);
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.broadcast.release(self.session.host());
    }
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;

    use ::kithara::platform::{CancelToken, time::Instant};
    use kithara_test_utils::{kithara, off_thread::OffThread};

    #[cfg(feature = "broadcast")]
    use crate::analysis::fixtures::tone_mp3;
    #[cfg(not(feature = "broadcast"))]
    use crate::{
        deck::DeckId,
        engine::{Command, DeckCmd, Envelope},
    };
    use crate::{gui::rig::Rig, pools::AppQueueControl};

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
            rig.queues.iter().all(AppQueueControl::is_closed),
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
    #[kithara::test(native, flash(false))]
    fn a_rung_picked_on_a_deck_without_a_ladder_publishes_the_ladder_mode() {
        let mut rig = Rig::offline();

        rig.engine.apply(Envelope {
            command: Command::Deck {
                deck: DeckId(0),
                cmd: DeckCmd::SetQuality(Some(1)),
            },
            seq: 1,
        });
        rig.engine.publish();

        let published = rig.snapshots.load();
        let stream = &published.deck(DeckId(0)).expect("deck A").stream;
        assert_eq!(
            (stream.selected, stream.is_auto),
            (None, true),
            "a deck with no ladder shows no pinned rung"
        );
    }
}
