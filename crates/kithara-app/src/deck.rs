use kithara::{
    abr::{AbrHandle, AbrMode},
    effects::{
        GainDb,
        eq::{EqBandConfig, generate_log_spaced_bands},
    },
    host::{DeckId, HostOwned},
    link::LinkedHostCommand,
    platform::CancelToken,
    play::{DeckMixerConfig, PlayError, ResourcePrep},
    queue::{QueueConfig, QueueError, Transition},
};
use kithara_config::Configure;
use tracing::{debug, error};

use crate::{
    config::AppConfig,
    engine::{DeckCmd, DeckSnapshot},
    mix::{MixState, MixStrip},
    pools::{self, AppHost, AppQueue, AppQueueControl},
    state::{GridPost, StateController},
};

/// EQ topology shared by every deck and its player graph.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum EqMode {
    #[default]
    ThreeBand,
    FourBand,
}

impl EqMode {
    const FOUR_BANDS: [&str; 4] = ["low", "low_mid", "high_mid", "high"];
    const THREE_BANDS: [&str; 3] = ["low", "mid", "high"];

    #[must_use]
    pub(crate) fn band(self, name: &str) -> Option<usize> {
        self.bands().iter().position(|band| *band == name)
    }

    #[must_use]
    pub(crate) const fn bands(self) -> &'static [&'static str] {
        match self {
            Self::ThreeBand => &Self::THREE_BANDS,
            Self::FourBand => &Self::FOUR_BANDS,
        }
    }

    #[must_use]
    pub(crate) fn layout(self, gains: &[GainDb]) -> Vec<EqBandConfig> {
        let mut layout = generate_log_spaced_bands(self.bands().len());

        for (band, gain) in layout.iter_mut().zip(gains) {
            band.set_gain_db(*gain);
        }

        layout
    }

    #[must_use]
    pub(crate) fn remap(self, next: Self, gains: &[GainDb]) -> Option<Vec<GainDb>> {
        match (self, next, gains) {
            (Self::ThreeBand, Self::FourBand, [low, mid, high]) => {
                Some(vec![*low, *mid, *mid, *high])
            }
            (Self::FourBand, Self::ThreeBand, [low, low_mid, high_mid, high]) => {
                Some(vec![*low, midpoint(*low_mid, *high_mid), *high])
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, kithara_derive::Ranged)]
#[ranged(min = -50.0, max = 50.0, default = 0.0, clamp)]
pub(crate) struct TempoPercent(pub(crate) f32);

impl TempoPercent {
    pub(crate) fn speed(self) -> f32 {
        1.0 + self.0 / 100.0
    }

    /// The tempo of a track playing at `speed`.
    pub(crate) fn at_speed(speed: f32) -> Self {
        Self::from((speed - 1.0) * 100.0)
    }
}

/// The gain halfway between two bands, for the four-to-three fold where two
/// mids collapse into one.
fn midpoint(low: GainDb, high: GainDb) -> GainDb {
    GainDb::from(f32::from(low).midpoint(f32::from(high)))
}

/// One deck on the engine thread. Dropping it cancels the deck's tasks.
pub struct DeckRecord {
    queue: HostOwned<AppQueue>,
    letter: &'static str,
    cancel: CancelToken,
    controller: StateController,
    strip: MixStrip,
    eq_bands: Vec<GainDb>,
    tempo: TempoPercent,
    synced: bool,
}

impl DeckRecord {
    /// Registers a new queue with the Host.
    ///
    /// # Errors
    /// Returns [`PlayError`] when the Host rejects the new deck.
    fn open(
        letter: &'static str,
        strip: MixStrip,
        config: &AppConfig,
        host: &mut AppHost,
        controller: impl FnOnce(&HostOwned<AppQueue>, &CancelToken, GridPost) -> StateController,
    ) -> Result<Self, PlayError> {
        let cancel = config.shutdown.child();

        let prep = ResourcePrep::builder()
            .worker(config.worker.clone())
            .warp(config.warp.clone())
            .cancel(cancel.child())
            .build();

        let mut queue_config = QueueConfig::with_factory(pools::factory(host.settings()))
            .prep(prep)
            .store(config.store.clone())
            .cancel(cancel.clone())
            .mixer(DeckMixerConfig::builder().eq_bands(config.eq_bands).build())
            .build();

        queue_config.apply(config.queue.clone()).map_err(|error| {
            PlayError::InvalidConfiguration {
                reason: error.to_string(),
            }
        })?;

        let queue = host.insert(AppQueue::new(queue_config))?;
        let grids = GridPost::new(queue.id(), host.poster());
        let controller = controller(&queue, &cancel, grids);
        let eq_bands = vec![GainDb::default(); queue.eq_band_count()];

        Ok(Self {
            queue,
            letter,
            cancel,
            controller,
            strip,
            eq_bands,
            tempo: TempoPercent::DEFAULT,
            synced: false,
        })
    }

    delegate::delegate! {
        to self.queue {
            #[must_use]
            pub fn id(&self) -> DeckId;
            #[call(control)]
            pub(crate) fn queue(&self) -> &AppQueueControl;
        }
    }

    pub(crate) fn refresh(&self) {
        self.controller.refresh_continuous();
    }

    pub(crate) fn snapshot(&self) -> DeckSnapshot {
        self.controller.read(|state| {
            DeckSnapshot::new(
                self.id(),
                self.letter,
                self.strip,
                self.eq_bands.clone(),
                self.tempo,
                self.synced,
                state,
            )
        })
    }

    /// Keeps the SYNC state the host accepted. Turned off, the deck takes the
    /// speed its track plays at as its tempo.
    fn sync_landed(&mut self, on: bool) {
        self.synced = on;
        if !on {
            self.set_tempo(TempoPercent::at_speed(self.queue().default_rate()));
        }
    }

    /// Seeks by the queue's own duration: the UI copy lags a tick behind.
    fn seek(&self, fraction: f64) {
        let queue = self.queue();
        let Some(duration) = queue.duration_seconds() else {
            debug!(fraction, "seek dropped: the track has no duration yet");
            return;
        };

        let target = fraction.clamp(0.0, 1.0) * duration.max(0.0);

        if let Err(e) = queue.seek(target) {
            error!("seek failed: {e:?}");
        }
    }

    fn set_eq_gain(&mut self, band: usize, gain: GainDb) {
        let Some(slot) = self.eq_bands.get_mut(band) else {
            return;
        };
        *slot = gain;

        if let Err(e) = self.queue().set_eq_gain(band, f32::from(gain)) {
            debug!(band, db = f32::from(gain), error = ?e, "set EQ gain deferred");
        }
    }

    /// Keeps `tempo` only once the deck took its speed.
    fn set_tempo(&mut self, tempo: TempoPercent) {
        match self.queue().set_rate(tempo.speed()) {
            Ok(()) => self.tempo = tempo,
            Err(e) => error!(speed = tempo.speed(), error = %e, "tempo refused"),
        }
    }

    /// `None` when the gains do not fit `current`.
    fn eq_mode_change(&self, current: EqMode, next: EqMode) -> Option<EqModeChange> {
        let Some(gains) = current.remap(next, &self.eq_bands) else {
            error!(
                deck = %self.id(),
                ?current,
                requested = ?next,
                bands = self.eq_bands.len(),
                "EQ mode state does not match its band layout"
            );
            return None;
        };

        Some(EqModeChange {
            previous: current.layout(&self.eq_bands),
            next: next.layout(&gains),
            gains,
        })
    }

    /// True when the queue took `layout`.
    fn set_eq_layout(&self, layout: &[EqBandConfig]) -> bool {
        self.queue()
            .set_eq_layout(layout.to_vec())
            .inspect_err(|error| error!(deck = %self.id(), ?error, "EQ layout refused"))
            .is_ok()
    }

    fn set_quality(&self, variant: Option<usize>) {
        let handle = self.queue().current_abr_handle();
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

impl Drop for DeckRecord {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

struct EqModeChange {
    gains: Vec<GainDb>,
    next: Vec<EqBandConfig>,
    previous: Vec<EqBandConfig>,
}

/// Owns the deck records and the global mix. A rejected mix edit changes nothing.
#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub struct DeckSet {
    #[field(get)]
    host: AppHost,
    #[field(get)]
    mix: MixState,
    #[field(get)]
    records: Vec<DeckRecord>,
}

impl DeckSet {
    /// Letters the engine gives its decks, in build order.
    pub(crate) const LETTERS: [&'static str; 2] = ["a", "b"];

    /// Opens one deck per letter.
    ///
    /// # Errors
    /// Returns [`PlayError`] when the Host rejects a deck.
    pub(crate) fn new(
        config: &AppConfig,
        mut host: AppHost,
        mut controller: impl FnMut(&HostOwned<AppQueue>, &CancelToken, GridPost) -> StateController,
    ) -> Result<Self, PlayError> {
        let count = Self::LETTERS.len();

        let records = Self::LETTERS
            .into_iter()
            .enumerate()
            .map(|(index, letter)| {
                let strip = MixStrip::new(index, count);
                DeckRecord::open(letter, strip, config, &mut host, &mut controller)
            })
            .collect::<Result<_, _>>()?;

        Ok(Self {
            host,
            records,
            mix: MixState::default(),
        })
    }

    pub(crate) fn close(&mut self) {
        for record in self.records.drain(..) {
            if let Err(error) = self.host.remove(&record.queue) {
                tracing::error!(
                    deck_id = %record.id(),
                    ?error,
                    "failed to remove deck during shutdown"
                );
            }
        }
    }

    /// Sends each deck its level from the stored mix.
    ///
    /// # Errors
    /// Returns [`QueueError`] when the mix is invalid or a deck refuses its level.
    pub fn send_levels(&mut self) -> Result<(), QueueError> {
        self.commit(self.mix, |record| record.strip)
    }

    /// Applies `mix` and the strips `edit` returns; stores them only when every
    /// deck accepts.
    ///
    /// # Errors
    /// Returns [`QueueError`] when the mix is invalid or a deck refuses its level.
    fn commit(
        &mut self,
        mix: MixState,
        edit: impl Fn(&DeckRecord) -> MixStrip,
    ) -> Result<(), QueueError> {
        for record in &self.records {
            mix.level(edit(record))?;
        }

        for record in &self.records {
            record.queue.set_level(mix.level(edit(record))?)?;
        }

        for record in &mut self.records {
            record.strip = edit(record);
        }

        self.mix = mix;
        Ok(())
    }

    pub(crate) fn record(&self, id: DeckId) -> Option<&DeckRecord> {
        self.records.iter().find(|record| record.id() == id)
    }

    /// Applies `cmd` to deck `id`.
    pub(crate) fn apply(&mut self, id: DeckId, cmd: DeckCmd, eq_mode: EqMode) {
        let Some(record) = self.records.iter_mut().find(|record| record.id() == id) else {
            return;
        };
        let queue = record.queue();

        match cmd {
            DeckCmd::TogglePlay if queue.is_playing() => queue.pause(),
            DeckCmd::TogglePlay => queue.play(),
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
            DeckCmd::SeekFraction(fraction) => record.seek(fraction),
            DeckCmd::SetEqGain { layout, band, gain } if layout == eq_mode => {
                record.set_eq_gain(band, gain);
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
            DeckCmd::SetTempo(tempo) => record.set_tempo(tempo),
            DeckCmd::SetQuality(variant) => record.set_quality(variant),
            DeckCmd::ToggleSync => Self::toggle_sync(&self.host, record),
        }
    }

    /// Waits for the host's answer; the deck keeps the change only once accepted.
    fn toggle_sync(host: &AppHost, record: &mut DeckRecord) {
        let deck = record.id();
        let on = !record.synced;
        match host.ask(LinkedHostCommand::Sync { deck, on }) {
            Ok(()) => record.sync_landed(on),
            Err(error) => error!(%deck, on, %error, "sync refused"),
        }
    }

    /// Move the DJ crossfader.
    ///
    /// # Errors
    /// Returns [`QueueError`] when the mix is invalid or a deck refuses its level.
    pub fn set_crossfader(&mut self, position: f32) -> Result<(), QueueError> {
        self.commit(
            MixState {
                position,
                ..self.mix
            },
            |record| record.strip,
        )
    }

    /// The deck's output fader: session-input gain, never content volume.
    ///
    /// # Errors
    /// Returns [`QueueError`] when the mix is invalid or a deck refuses its level.
    pub fn set_trim(&mut self, id: DeckId, trim: f32) -> Result<(), QueueError> {
        self.commit(self.mix, |record| {
            if record.id() == id {
                MixStrip {
                    trim,
                    ..record.strip
                }
            } else {
                record.strip
            }
        })
    }

    /// Switches every deck to `next`, or none of them; true on success.
    pub(crate) fn set_eq_mode(&mut self, current: EqMode, next: EqMode) -> bool {
        let changes: Option<Vec<EqModeChange>> = self
            .records
            .iter()
            .map(|record| record.eq_mode_change(current, next))
            .collect();
        let Some(changes) = changes else {
            return false;
        };

        let pairs = self.records.iter().zip(&changes);
        for (applied, (record, change)) in pairs.clone().enumerate() {
            if !record.set_eq_layout(&change.next) {
                for (record, change) in pairs.take(applied).rev() {
                    record.set_eq_layout(&change.previous);
                }
                return false;
            }
        }

        for (record, change) in self.records.iter_mut().zip(changes) {
            record.eq_bands = change.gains;
        }

        true
    }
}

impl Drop for DeckSet {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(all(test, feature = "mock"))]
pub(crate) mod mock {
    use super::{AppHost, DeckSet};

    pub(crate) fn host(set: &mut DeckSet) -> &mut AppHost {
        &mut set.host
    }
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;

    use kithara::{
        host::HostConfig,
        platform::{
            sync::{Arc, Mutex},
            thread,
            time::{Duration, WallInstant},
        },
    };
    use kithara_test_utils::off_thread::OffThread;

    use super::*;
    use crate::{
        analysis::fixtures::{self, constant_fifth_wav, constant_tenth_wav},
        gui::rig::Rig,
        mock::Capture,
        state::{UiState, test_fixture::controller_on},
    };

    async fn deck_set() -> OffThread<DeckSet> {
        OffThread::spawn("app-host", || {
            Ok::<_, Infallible>(open_deck_set(&CancelToken::root()))
        })
        .await
        .expect("deck set fixture is infallible")
    }

    fn open_deck_set(shutdown: &CancelToken) -> DeckSet {
        let config = fixtures::app_config(shutdown, fixtures::memory_store());
        let host = pools::host(HostConfig::offline(config.worker.pools().clone()).build())
            .expect("test host");

        DeckSet::new(&config, host, |queue, cancel, _grids| {
            let state = Arc::new(Mutex::new(UiState::new(queue.control())));
            controller_on(queue.control().clone(), cancel.child(), state)
        })
        .expect("host accepts the decks")
    }

    fn id(set: &DeckSet, letter: &str) -> DeckId {
        set.records
            .iter()
            .find(|record| record.letter == letter)
            .map(DeckRecord::id)
            .expect("the set holds the deck")
    }

    #[kithara::test]
    fn speed_is_one_percent_per_tempo_point() {
        let speed = |tempo| TempoPercent::from(tempo).speed();

        assert!((speed(0.0) - 1.0).abs() < f32::EPSILON);
        assert!((speed(TempoPercent::MAX.0) - 1.5).abs() < 1e-6);
        assert!((speed(TempoPercent::MIN.0) - 0.5).abs() < 1e-6);
    }

    #[kithara::test]
    fn a_played_speed_is_kept_as_its_tempo() {
        let played = 124.0 / 120.0;
        assert!((TempoPercent::at_speed(played).speed() - played).abs() < 1e-6);
    }

    #[kithara::test]
    fn tempo_percent_clamps_controls_and_rejects_non_finite_documents() {
        assert_eq!(TempoPercent::from(-80.0), TempoPercent::MIN);
        assert_eq!(TempoPercent::from(80.0), TempoPercent::MAX);
        assert_eq!(TempoPercent::from(f32::NAN), TempoPercent::DEFAULT);
        assert!(TempoPercent::checked(f32::INFINITY).is_none());
    }

    #[kithara::test(native, flash(false))]
    fn eq_mode_maps_the_middle_band_without_moving_the_outer_bands() {
        let four = EqMode::ThreeBand
            .remap(EqMode::FourBand, &[-6.0f32, 2.0, 5.0].map(GainDb::from))
            .unwrap();
        assert_eq!(four, [-6.0f32, 2.0, 2.0, 5.0].map(GainDb::from));

        let three = EqMode::FourBand
            .remap(
                EqMode::ThreeBand,
                &[-6.0f32, -2.0, 4.0, 5.0].map(GainDb::from),
            )
            .unwrap();
        assert_eq!(three, [-6.0f32, 1.0, 5.0].map(GainDb::from));
    }

    #[kithara::test(native, tokio, flash(false))]
    async fn decks_own_independent_players_and_share_one_session() {
        let set = deck_set().await;
        set.call(|set| {
            assert_eq!(set.records().len(), DeckSet::LETTERS.len());

            for (i, a) in set.records().iter().enumerate() {
                for b in set.records().iter().skip(i + 1) {
                    assert_ne!(a.id(), b.id());
                }
            }

            // Only accepted if they share one session (a foreign one is rejected).
            set.send_levels().expect("all decks share one session");
        })
        .await;
        set.close().await;
    }

    #[kithara::test(native, tokio, flash(false))]
    async fn failed_apply_rolls_the_app_mix_back() {
        let set = deck_set().await;
        set.call(|set| {
            set.set_crossfader(0.25).expect("valid crossfader");
            let before = *set.mix();

            let err = set.set_crossfader(1.5).expect_err("invalid position");
            assert!(matches!(
                err,
                QueueError::Play(PlayError::MixPosition { .. })
            ));
            assert_eq!(set.mix(), &before);

            let deck = id(set, "b");
            let strip = set.record(deck).map(|record| record.strip);
            let err = set
                .set_trim(deck, f32::NAN)
                .expect_err("a trim that is not a number");
            assert!(matches!(err, QueueError::Play(PlayError::MixLevel { .. })));
            assert_eq!(set.record(deck).map(|record| record.strip), strip);
        })
        .await;
        set.close().await;
    }

    const DECK_A: f32 = 0.2;
    const DECK_B: f32 = 0.1;
    /// The share of a bus the crossfader centre passes: -3 dB.
    const CENTRE: f32 = std::f32::consts::FRAC_1_SQRT_2;
    const TOL: f32 = 1e-3;

    /// The value every sample of `block` holds, if they hold one.
    fn plateau(block: &[f32]) -> Option<f32> {
        let (first, rest) = block.split_first()?;
        rest.iter()
            .all(|sample| (sample - first).abs() < TOL)
            .then_some(*first)
    }

    struct Playing {
        set: DeckSet,
        shutdown: CancelToken,
        rendered: u64,
    }

    impl Playing {
        const BLOCK: u64 = 1024;

        fn open(a: &str, b: &str) -> Self {
            let shutdown = CancelToken::root();
            let set = open_deck_set(&shutdown);

            for (record, url) in set.records().iter().zip([a, b]) {
                let queue = record.queue();
                let track = queue.append(url).expect("the deck takes the track");
                queue
                    .select(track, Transition::None)
                    .expect("the deck selects the track");
                queue.play();
            }

            Self {
                set,
                shutdown,
                rendered: 0,
            }
        }

        fn block(&mut self) -> Capture {
            let end = self.rendered + Self::BLOCK;
            let mut block = Capture::default();
            crate::mock::render_into(
                mock::host(&mut self.set),
                self.rendered..end,
                &self.shutdown,
                &mut block,
            )
            .expect("the offline mix renders");
            self.rendered = end;
            block
        }

        /// Renders until a block holds `expected`; a block holding one of
        /// `wrong` fails.
        fn sounds(&mut self, what: &str, expected: f32, wrong: &[f32]) {
            let deadline = WallInstant::now() + Rig::DEADLINE;

            loop {
                if let Some(level) = plateau(self.block().samples()) {
                    assert!(
                        wrong.iter().all(|value| (level - value).abs() >= TOL),
                        "{what}: a block holds {level}, the mix of a wrong level"
                    );
                    if (level - expected).abs() < TOL {
                        return;
                    }
                }

                assert!(
                    WallInstant::now() < deadline,
                    "{what}: no block holds {expected} within {:?}",
                    Rig::DEADLINE
                );
                thread::paced_backoff(Duration::from_millis(5));
            }
        }
    }

    async fn playing(a: String, b: String) -> OffThread<Playing> {
        OffThread::spawn("app-host", move || {
            Ok::<_, Infallible>(Playing::open(&a, &b))
        })
        .await
        .expect("playing deck set fixture is infallible")
    }

    #[kithara::test(native, tokio, flash(false))]
    async fn a_crossfader_move_sounds_the_deck_on_its_side(
        constant_fifth_wav: String,
        constant_tenth_wav: String,
    ) {
        let decks = playing(constant_fifth_wav, constant_tenth_wav).await;
        decks
            .call(|decks| {
                decks.set.set_crossfader(0.0).expect("crossfader to A");
                decks.sounds("crossfader at A", DECK_A, &[DECK_B, DECK_A + DECK_B]);

                decks.set.set_crossfader(1.0).expect("crossfader to B");
                decks.sounds("crossfader at B", DECK_B, &[DECK_A, DECK_A + DECK_B]);
            })
            .await;
        decks.close().await;
    }

    #[kithara::test(native, tokio, flash(false))]
    async fn a_trim_scales_its_deck_alone_and_leaves_its_content_volume(
        constant_fifth_wav: String,
        constant_tenth_wav: String,
    ) {
        let decks = playing(constant_fifth_wav, constant_tenth_wav).await;
        decks
            .call(|decks| {
                decks
                    .set
                    .set_trim(id(&decks.set, "b"), 0.25)
                    .expect("trim deck b");
                decks.sounds(
                    "deck b trimmed at the crossfader centre",
                    CENTRE * (DECK_A + 0.25 * DECK_B),
                    &[
                        DECK_A + DECK_B,
                        CENTRE * (DECK_A + DECK_B),
                        CENTRE * (0.25 * DECK_A + DECK_B),
                        CENTRE * 0.25 * (DECK_A + DECK_B),
                    ],
                );

                for record in decks.set.records() {
                    assert_eq!(record.queue().volume(), 1.0);
                }
            })
            .await;
        decks.close().await;
    }
}
