use arc_swap::ArcSwap;
use kithara_audio::AudioObserver;
use kithara_bufpool::HasPool;
use kithara_events::{EventReceiver, EventSet, TrackId};
use kithara_platform::sync::Arc;
use kithara_play::{
    DeckMixSettings, DeckMixerConfig, DeckSnapshot, EngineLoadSnapshot, Player, PlayerStatus,
    Position, SlotSnapshot, Track, TrackFactory, TrackSettings, TrackSnapshot,
    TrackStatus as PlayingStatus,
};

use super::{PlaybackView, Queue, QueueControl, types::SeekPosition};
use crate::{
    ActionAtItemEnd, NavigationState, PlaybackOrder, QueueSettings, RepeatMode, TrackEntry,
    TrackSource, TrackStatus,
    track::{TrackRecord, TrackRow, Tracks},
};

#[derive(Clone)]
pub(super) struct DeckObservation {
    pub(super) mix: DeckMixSettings,
    pub(super) mixer: DeckSnapshot,
    pub(super) suspended: bool,
}

impl DeckObservation {
    pub(super) fn new(config: DeckMixerConfig) -> Self {
        Self {
            mix: config.mix(),
            mixer: DeckSnapshot::new(config),
            suspended: false,
        }
    }
}

/// The queue's published rows, sounding track and navigation settings.
#[derive_where::derive_where(Clone)]
pub struct QueueSnapshot<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    pub current: Option<TrackId>,
    pub(super) upcoming: Option<TrackId>,
    pub track: Option<TrackSnapshot>,
    follows_host: Option<bool>,
    pub settings: QueueSettings,
    rows: Arc<[TrackRow<S>]>,
    revision: u64,
    order: PlaybackOrder,
    repeat: RepeatMode,
    action: ActionAtItemEnd,
    initial: TrackSettings,
    slot: Option<SlotSnapshot>,
    sample_rate: u32,
    held_position: Option<Position>,
    pub(super) deck: DeckObservation,
    engine_load: EngineLoadSnapshot,
}

impl<S> QueueSnapshot<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    pub(super) fn applied_rate(&self) -> f32 {
        if self.deck.suspended {
            return 0.0;
        }
        self.track
            .as_ref()
            .filter(|track| matches!(track.status, PlayingStatus::Playing { .. }))
            .map_or(0.0, |track| track.speed)
    }
}

/// Handles only read this snapshot; the queue is its sole publisher.
#[derive_where::derive_where(Clone)]
pub(crate) struct QueueView<S>(Arc<ArcSwap<QueueSnapshot<S>>>)
where
    S: HasPool<u8> + Send + Sync + 'static;

impl<S> QueueView<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    pub(crate) fn new(
        tracks: &Tracks<S>,
        navigation: &NavigationState,
        settings: QueueSettings,
        initial: TrackSettings,
        action: ActionAtItemEnd,
        deck: DeckObservation,
    ) -> Self {
        Self(Arc::new(ArcSwap::from_pointee(QueueSnapshot {
            current: None,
            upcoming: None,
            track: None,
            follows_host: None,
            settings,
            rows: tracks.rows(),
            revision: tracks.revision(),
            order: navigation.playback_order(),
            repeat: navigation.repeat_mode(),
            action,
            initial,
            slot: None,
            sample_rate: 0,
            held_position: None,
            deck,
            engine_load: EngineLoadSnapshot::default(),
        })))
    }

    pub(super) fn read(&self) -> Arc<QueueSnapshot<S>> {
        self.0.load_full()
    }

    pub(super) fn publish(&self, snapshot: QueueSnapshot<S>) {
        self.0.store(Arc::new(snapshot));
    }

    pub(super) fn attach_observer(&self, id: TrackId, observer: Box<dyn AudioObserver>) {
        if let Some(row) = self.0.load().rows.iter().find(|row| row.entry.id == id) {
            row.observer.attach(observer);
        }
    }
}

impl<S, F> Queue<S, F>
where
    S: HasPool<u8> + Send + Sync + 'static,
    F: TrackFactory<S>,
{
    /// The transition target, or the track navigation advances to.
    fn upcoming(&mut self) -> Option<TrackId> {
        self.target.map(|target| target.to).or_else(|| {
            let ids = self.selectable_ids();
            let wrap = self.navigation.repeat_mode() == RepeatMode::All;
            self.navigation.next(&ids, self.current, true, wrap)
        })
    }

    pub(super) fn queue_snapshot(&mut self) -> QueueSnapshot<S> {
        let upcoming = self.upcoming();

        let shown = self
            .shown_index()
            .and_then(|index| self.active.get(index))
            .map(|active| &active.track);
        let follows_host = shown.map(Track::follows_host);
        let mut track = shown.map(|track| track.snapshot().as_ref().clone());
        if let Some(track) = &mut track {
            match self.held_position {
                Some(SeekPosition::Held(to)) => track.position = to,
                Some(SeekPosition::Pending { item, to, .. }) if track.item == item => {
                    track.position = to;
                }
                _ => {}
            }
        }
        let slot = track
            .as_ref()
            .and_then(|track| track.slot)
            .and_then(|slot| self.deck.mixer.slots.get(usize::from(slot.get())).copied());
        let published = self.view.read();
        let rows = if published.revision == self.tracks.revision() {
            Arc::clone(&published.rows)
        } else {
            self.tracks.rows()
        };
        QueueSnapshot {
            current: self.current,
            upcoming,
            track,
            follows_host,
            settings: self.config.settings,
            rows,
            revision: self.tracks.revision(),
            order: self.navigation.playback_order(),
            repeat: self.navigation.repeat_mode(),
            action: self.config.action_at_item_end,
            initial: self.config.track,
            slot,
            sample_rate: self.deck.mixer.sample_rate,
            held_position: self.held_position.and_then(SeekPosition::held),
            deck: self.deck.clone(),
            engine_load: self
                .config
                .prep
                .as_ref()
                .map_or_else(EngineLoadSnapshot::default, |prep| {
                    prep.engine_load.snapshot()
                }),
        }
    }

    #[must_use]
    pub fn current(&self) -> Option<TrackEntry> {
        self.track(self.current?)
    }

    #[must_use]
    pub fn current_index(&self) -> Option<usize> {
        let id = self.current?;
        self.tracks
            .records()
            .iter()
            .position(|record| record.id == id)
    }

    #[must_use]
    pub fn track(&self, id: TrackId) -> Option<TrackEntry> {
        self.tracks
            .records()
            .iter()
            .find(|record| record.id == id)
            .map(TrackRecord::entry)
    }

    #[must_use]
    pub fn tracks(&self) -> Vec<TrackEntry> {
        self.tracks
            .records()
            .iter()
            .map(TrackRecord::entry)
            .collect()
    }

    #[must_use]
    pub fn subscribe<E: EventSet>(&self) -> EventReceiver<E> {
        self.bus.subscribe()
    }
}

impl<S> QueueControl<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    #[must_use]
    pub fn current(&self) -> Option<TrackEntry> {
        let snapshot = self.view.read();
        let id = snapshot.current?;
        snapshot
            .rows
            .iter()
            .find(|row| row.entry.id == id)
            .map(|row| row.entry.clone())
    }

    #[must_use]
    pub fn current_index(&self) -> Option<usize> {
        let snapshot = self.view.read();
        let id = snapshot.current?;
        snapshot.rows.iter().position(|row| row.entry.id == id)
    }

    /// The track the queue enters next: its transition target, or the one
    /// navigation advances to.
    #[must_use]
    pub fn upcoming(&self) -> Option<TrackId> {
        self.view.read().upcoming
    }

    #[must_use]
    pub fn track(&self, id: TrackId) -> Option<TrackEntry> {
        self.view
            .read()
            .rows
            .iter()
            .find(|row| row.entry.id == id)
            .map(|row| row.entry.clone())
    }

    #[must_use]
    pub fn tracks(&self) -> Vec<TrackEntry> {
        self.view
            .read()
            .rows
            .iter()
            .map(|row| row.entry.clone())
            .collect()
    }

    #[must_use]
    pub fn track_source(&self, id: TrackId) -> Option<TrackSource<S>> {
        self.view
            .read()
            .rows
            .iter()
            .find(|row| row.entry.id == id)
            .map(|row| row.source.clone())
    }

    pub fn attach_observer<O: AudioObserver>(&self, id: TrackId, observer: O) {
        self.view.attach_observer(id, Box::new(observer));
    }

    #[must_use]
    pub fn subscribe<E: EventSet>(&self) -> EventReceiver<E> {
        self.bus.subscribe()
    }

    #[must_use]
    pub fn bus(&self) -> &kithara_events::EventBus {
        &self.bus
    }

    delegate::delegate! {
        to self.view.read() {
            #[must_use]
            #[call(applied_rate)]
            pub fn rate(&self) -> f32;
        }
        to self.view.read().rows {
            #[must_use]
            pub fn len(&self) -> usize;
            #[must_use]
            pub fn is_empty(&self) -> bool;
        }
        to self.view {
            #[must_use]
            #[expr($.order)]
            #[call(read)]
            pub fn playback_order(&self) -> PlaybackOrder;
            #[must_use]
            #[expr($.repeat)]
            #[call(read)]
            pub fn repeat_mode(&self) -> RepeatMode;
            #[must_use]
            #[expr($.action)]
            #[call(read)]
            pub fn action_at_item_end(&self) -> ActionAtItemEnd;
            #[must_use]
            #[expr($.sample_rate)]
            #[call(read)]
            pub fn sample_rate(&self) -> u32;
            /// Blocks rendered by this deck's mixer, published by its Host pass.
            #[must_use]
            #[expr($.deck.mixer.blocks)]
            #[call(read)]
            pub fn mixer_blocks(&self) -> u64;
            /// Real-time counters from the same published mixer observation.
            #[must_use]
            #[expr($.deck.mixer.metrics)]
            #[call(read)]
            pub fn mixer_metrics(&self) -> kithara_play::RtMetricsSnapshot;
            #[must_use]
            #[expr($.engine_load)]
            #[call(read)]
            pub fn engine_load(&self) -> EngineLoadSnapshot;
        }
        to self.view.read().deck.mix {
            #[must_use]
            #[expr(f32::from($))]
            pub fn volume(&self) -> f32;
            #[must_use]
            #[call(muted)]
            pub fn is_muted(&self) -> bool;
        }
        to self.view.read().deck.mixer.eq {
            #[must_use]
            #[call(bands)]
            pub fn eq_band_count(&self) -> usize;
            #[must_use]
            #[expr($.map(f32::from))]
            #[call(gain)]
            pub fn eq_gain(&self, band: usize) -> Option<f32>;
        }
    }

    #[must_use]
    pub fn crossfade_settings(&self) -> kithara_play::CrossfadeSettings {
        self.view.read().settings.crossfade()
    }

    #[must_use]
    pub fn is_playing(&self) -> bool {
        let snapshot = self.view.read();
        !snapshot.deck.suspended
            && snapshot
                .track
                .as_ref()
                .is_some_and(|track| matches!(track.status, PlayingStatus::Playing { .. }))
    }

    /// Whether the host's tempo drives the current track; `None` with no track.
    #[must_use]
    pub fn follows_host(&self) -> Option<bool> {
        self.view.read().follows_host
    }

    #[must_use]
    pub fn default_rate(&self) -> f32 {
        let snapshot = self.view.read();
        snapshot
            .track
            .as_ref()
            .map_or_else(|| snapshot.initial.speed(), |track| track.speed)
    }

    #[must_use]
    pub fn position_seconds(&self) -> Option<f64> {
        let snapshot = self.view.read();
        snapshot
            .track
            .as_ref()
            .map(|track| track.position.as_secs_f64())
            .or_else(|| {
                snapshot
                    .held_position
                    .map(|position| position.as_secs_f64())
            })
    }

    #[must_use]
    pub fn duration_seconds(&self) -> Option<f64> {
        let snapshot = self.view.read();
        snapshot
            .track
            .as_ref()
            .and_then(|track| track.duration.map(|duration| duration.as_secs_f64()))
    }

    #[must_use]
    pub fn status(&self) -> PlayerStatus {
        let snapshot = self.view.read();
        if snapshot.current.is_some_and(|id| {
            snapshot
                .rows
                .iter()
                .any(|row| row.entry.id == id && matches!(row.entry.status, TrackStatus::Failed(_)))
        }) {
            PlayerStatus::Failed
        } else if snapshot.track.as_ref().is_some_and(|track| {
            !matches!(
                track.status,
                PlayingStatus::Idle | PlayingStatus::Loading | PlayingStatus::Released
            )
        }) {
            PlayerStatus::ReadyToPlay
        } else {
            PlayerStatus::Unknown
        }
    }

    #[must_use]
    pub fn current_abr_handle(&self) -> Option<kithara_abr::AbrHandle> {
        self.view
            .read()
            .track
            .as_ref()
            .and_then(|track| track.abr.clone())
    }

    #[must_use]
    pub fn current_variant(&self) -> Option<kithara_abr::VariantInfo> {
        self.current_abr_handle()?.current_variant()
    }

    #[must_use]
    pub fn playback_view(&self) -> PlaybackView {
        let snapshot = self.view.read();
        let Some(track) = &snapshot.track else {
            return PlaybackView::default();
        };
        PlaybackView {
            buffered: snapshot.slot.map(|slot| slot.frontier.max(slot.cached)),
            duration: track.duration.map(|duration| duration.as_secs_f64()),
            position: Some(track.position.as_secs_f64()),
            playing: !snapshot.deck.suspended
                && matches!(track.status, PlayingStatus::Playing { .. }),
        }
    }
}

#[cfg(test)]
mod tests {
    use kithara_platform::time::Duration;
    use kithara_play::{
        PlayWorker, PlayWorkerConfig, PlayerConfig, PlayerFactory, PlayerImpl, ResourcePrep, Slot,
        TrackFactory,
    };
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        QueueConfig,
        test_pools::{TestPools, pools},
    };

    fn view_of(frontier: f64, cached: f64) -> PlaybackView {
        let prep = ResourcePrep::builder()
            .worker(PlayWorker::new(PlayWorkerConfig::builder(pools()).build()))
            .build();
        let queue = Queue::new(QueueConfig::builder().prep(prep).build());
        let player: PlayerImpl<TestPools> = PlayerFactory
            .track(PlayerConfig {
                item: TrackId::allocate(),
                slot: Some(Slot::new(0)),
                settings: TrackSettings::default(),
            })
            .expect("valid track settings");
        let mut snapshot = (*queue.view.read()).clone();
        let mut track = player.snapshot();
        track.duration = Some(Duration::from_secs(200));
        snapshot.track = Some(track);
        snapshot.slot = Some(SlotSnapshot {
            frontier,
            cached,
            ..SlotSnapshot::default()
        });
        queue.view.publish(snapshot);
        queue.control().playback_view()
    }

    #[kithara::test]
    fn buffered_covers_the_cached_span() {
        let view = view_of(4.0, 120.0);
        assert_eq!(view.buffered, Some(120.0));
        assert_eq!(view.duration, Some(200.0));
    }

    #[kithara::test]
    fn buffered_never_falls_behind_the_decoded_frontier() {
        assert_eq!(view_of(90.0, 12.0).buffered, Some(90.0));
    }

    #[kithara::test]
    fn buffered_is_zero_when_nothing_is_available() {
        assert_eq!(view_of(0.0, 0.0).buffered, Some(0.0));
    }

    #[kithara::test]
    fn playback_shared_defaults() {
        let prep = ResourcePrep::builder()
            .worker(PlayWorker::new(PlayWorkerConfig::builder(pools()).build()))
            .build();
        let queue: Queue<TestPools> = Queue::new(QueueConfig::builder().prep(prep).build());
        let control = queue.control();
        let snapshot = control.view.read();

        assert!(!control.is_playing());
        assert!(
            snapshot
                .track
                .as_ref()
                .and_then(|track| track.mark)
                .is_none()
        );
        assert_eq!(control.position_seconds(), None);
        assert_eq!(control.duration_seconds(), None);
        assert_eq!(control.rate(), 0.0);
        assert_eq!(snapshot.sample_rate, 0);
    }

    #[kithara::test]
    fn snapshot_reads_all_fields_at_once() {
        let prep = ResourcePrep::builder()
            .worker(PlayWorker::new(PlayWorkerConfig::builder(pools()).build()))
            .build();
        let queue: Queue<TestPools> = Queue::new(QueueConfig::builder().prep(prep).build());
        let player: PlayerImpl<TestPools> = PlayerFactory
            .track(PlayerConfig {
                item: TrackId::allocate(),
                slot: Some(Slot::new(0)),
                settings: TrackSettings::default(),
            })
            .expect("valid track settings");
        let mut published = (*queue.view.read()).clone();
        let mut track = player.snapshot();
        track.status = PlayingStatus::Playing {
            since: kithara_signal::SessionFrame::new(0),
        };
        track.position = Duration::from_secs(12);
        track.duration = Some(Duration::from_secs(180));
        track.speed = 1.25;
        published.track = Some(track);
        published.slot = Some(SlotSnapshot {
            position: 12.0,
            frontier: 20.0,
            ..SlotSnapshot::default()
        });
        published.sample_rate = 48_000;
        queue.view.publish(published);

        let snapshot = queue.control().view.read();
        let track = snapshot.track.as_ref().expect("published track");
        let slot = snapshot.slot.expect("published mixer slot");
        assert!(!snapshot.deck.suspended && matches!(track.status, PlayingStatus::Playing { .. }));
        assert!((track.position.as_secs_f64() - 12.0).abs() < f64::EPSILON);
        assert!((slot.frontier - 20.0).abs() < f64::EPSILON);
        assert!(
            (track.duration.expect("published duration").as_secs_f64() - 180.0).abs()
                < f64::EPSILON
        );
        assert!((track.speed - 1.25).abs() < f32::EPSILON);
        assert_eq!(snapshot.sample_rate, 48_000);
    }
}
