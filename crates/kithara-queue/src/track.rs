use std::vec;

use kithara_audio::{AudioObserver, AudioObserverSlot};
use kithara_bufpool::HasPool;
use kithara_decode::TrackMetadata;
use kithara_events::TrackId;
use kithara_platform::{CancelToken, sync::Arc};
use kithara_play::{ResourceConfig, ResourceSrc};
use tracing::debug;

use crate::{
    error::QueueError,
    event::{QueueEvent, TrackStatus},
    loading::{LoadReport, TrackLoad},
};

/// Snapshot of a track entry in the queue.
#[derive(Debug, Clone, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
#[non_exhaustive]
pub struct TrackEntry {
    /// Canonical source location: a normalized URL or a file path.
    /// `None` only for a non-UTF-8 file path.
    pub url: Option<String>,
    /// Display name derived from the URL or caller-supplied. May be empty.
    pub name: String,
    /// The source's metadata, with its unset fields filled from the decoder's
    /// tags once the track's resource is admitted; the cover read from the
    /// source's artwork becomes its artwork.
    #[field(get)]
    metadata: TrackMetadata,
    /// Stable identifier.
    pub id: TrackId,
    /// Current loading status.
    pub status: TrackStatus,
}

/// Cloneable input to [`QueueControl::append`](crate::QueueControl::append) and [`QueueControl::insert`](crate::QueueControl::insert).
/// Uri builds from queue templates; Config preserves caller fields such as DRM, headers and format hints.
/// Cloning lets the queue load a track again each time it plays, without caller reconstruction.
#[derive(derive_more::From)]
#[non_exhaustive]
#[derive_where::derive_where(Clone; S: HasPool<u8> + Send + Sync + 'static)]
pub enum TrackSource<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    /// Load from URL / path. Queue fills in defaults from `QueueConfig`.
    #[from]
    Uri(String),
    /// Caller-assembled resource config (DRM, headers, etc.). Boxed because
    /// [`ResourceConfig`] is ~100 bytes larger than the `Uri` variant.
    #[from]
    Config(Box<ResourceConfig<S>>),
}

impl<S> TrackSource<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    /// Canonical source location: the string for [`TrackSource::Uri`], the
    /// config's URL or file path for [`TrackSource::Config`]. `None` only
    /// for a non-UTF-8 file path.
    #[must_use]
    pub fn uri(&self) -> Option<&str> {
        match self {
            Self::Uri(s) => Some(s),
            Self::Config(cfg) => match cfg.source() {
                ResourceSrc::Url(url) => Some(url.as_str()),
                ResourceSrc::Path(path) => path.to_str(),
            },
        }
    }

    pub(crate) fn metadata(&self) -> Option<&TrackMetadata> {
        match self {
            Self::Config(config) => config.metadata(),
            Self::Uri(_) => None,
        }
    }
}

impl<S> From<&str> for TrackSource<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    fn from(s: &str) -> Self {
        Self::Uri(s.to_string())
    }
}

impl<S> From<ResourceConfig<S>> for TrackSource<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    fn from(c: ResourceConfig<S>) -> Self {
        Self::Config(Box::new(c))
    }
}

/// Single owner of everything the queue knows about one track. Dropping the
/// record aborts its load via [`TrackLoad`].
pub(crate) struct TrackRecord<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    pub(crate) load: Option<TrackLoad>,
    pub(crate) url: Option<String>,
    pub(crate) name: String,
    /// Taken from the source at append; a load fills its unset fields.
    pub(crate) metadata: TrackMetadata,
    pub(crate) id: TrackId,
    pub(crate) source: TrackSource<S>,
    pub(crate) status: TrackStatus,
    observer: AudioObserverSlot,
}

impl<S> TrackRecord<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    pub(crate) fn new(id: TrackId, name: String, source: TrackSource<S>) -> Self {
        Self {
            id,
            name,
            metadata: source.metadata().cloned().unwrap_or_default(),
            url: source.uri().map(str::to_string),
            status: TrackStatus::Pending,
            source,
            load: None,
            observer: AudioObserverSlot::default(),
        }
    }

    pub(crate) fn entry(&self) -> TrackEntry {
        TrackEntry {
            id: self.id,
            name: self.name.clone(),
            metadata: self.metadata.clone(),
            url: self.url.clone(),
            status: self.status.clone(),
        }
    }

    /// Whether the track's load was cancelled since it began.
    fn load_cancelled(&self) -> bool {
        self.load
            .as_ref()
            .is_some_and(|load| load.token().is_cancelled())
    }
}

/// One track as the queue's handles read it: what [`TrackEntry`] shows, the
/// source to rebuild it from, and the slot that reaches its decoder.
pub(crate) struct TrackRow<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    pub(crate) entry: TrackEntry,
    pub(crate) source: TrackSource<S>,
    pub(crate) observer: AudioObserverSlot,
}

/// Authoritative store for the queue's track list.
///
/// Single owner of `Vec<TrackRecord>`, held by the [`Queue`](crate::Queue)
/// alone; what becomes of a track's load reaches it through the deck track
/// that opened it. Every status transition MUST go through
/// [`Tracks::set_status`] (or the load ops below), which records its
/// [`QueueEvent::TrackStatusChanged`] for the queue to announce, so the
/// published rows and the event stream never drift. Every edit moves
/// [`Tracks::revision`], which tells the queue the rows it published are out
/// of date.
#[derive_where::derive_where(Default)]
#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(crate) struct Tracks<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    /// Moves with every edit; equal revisions mean equal rows.
    #[field(get, vis = "pub(crate)", copy)]
    revision: u64,
    /// The changes made since the queue last took them, in order.
    events: Vec<QueueEvent>,
    /// The records, in queue order.
    #[field(get, vis = "pub(crate)")]
    records: Vec<TrackRecord<S>>,
}

impl<S> Tracks<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    /// Apply what a task beside a track's load reported.
    pub(crate) fn apply_report(&mut self, report: LoadReport) {
        match report {
            LoadReport::Slow { id, watch } => {
                if !watch.is_cancelled() {
                    self.set_status(id, TrackStatus::Slow);
                }
            }
            LoadReport::Cover { id, load, cover } => self.place_cover(id, &load, cover),
        }
    }

    /// Attach decoded-audio observation to this track's decoder, or retain it
    /// for the next load when loading has not started yet.
    #[cfg(test)]
    pub(crate) fn attach_observer(&self, id: TrackId, observer: Box<dyn AudioObserver>) {
        if let Some(record) = self.find(id) {
            record.observer.attach(observer);
        }
    }

    delegate::delegate! {
        to self {
            /// What reaches `id`'s decoder from the observers attached to it.
            #[expr($.map(|record| Box::new(record.observer.relay()) as Box<dyn AudioObserver>))]
            #[call(find)]
            pub(crate) fn observer(&self, id: TrackId) -> Option<Box<dyn AudioObserver>>;
            /// Original source for `id`, if still queued.
            #[expr($.map(|record| record.source.clone()))]
            #[call(find)]
            pub(crate) fn source(&self, id: TrackId) -> Option<TrackSource<S>>;
        }
    }

    /// `id` loads under `load`: a load it replaces is cancelled.
    pub(crate) fn begin_load(&mut self, id: TrackId, load: TrackLoad) {
        let Some(record) = self.record_mut(id) else {
            return;
        };
        record.load = Some(load);
        if !matches!(record.status, TrackStatus::Loading | TrackStatus::Slow) {
            self.set_status(id, TrackStatus::Loading);
        }
    }

    /// `id`'s track opened on the deck or in the background: the token passes to its
    /// resource, the metadata the caller left unset fills from the decoder's
    /// tags, and the track is `Loaded`. `false` when the load was cancelled
    /// meanwhile: the cancel is the last word on it, and the track is left
    /// `Cancelled` for the queue to let go. An already `Loaded` record also
    /// returns `false` without announcing another status change.
    pub(crate) fn loaded(&mut self, id: TrackId, metadata: &TrackMetadata) -> bool {
        let Some(record) = self.record_mut(id) else {
            return false;
        };
        if record.status == TrackStatus::Loaded {
            return false;
        }
        if record.load_cancelled() {
            self.set_status(id, TrackStatus::Cancelled);
            return false;
        }
        if let Some(mut load) = record.load.take() {
            load.disarm();
        }
        record.metadata.fill_missing_from(metadata);
        self.set_status(id, TrackStatus::Loaded);
        true
    }

    /// `id`'s open was refused with `error`. Returns whether to ask again.
    ///
    /// A cancel is the last word on a load, whatever its open ended with. A
    /// refusal a later ask can answer keeps a `wanted` track loading, so a
    /// track chosen during an outage plays once the network answers; a load
    /// nobody waits for fails. A track no longer loading has nothing to fail.
    pub(crate) fn refused(
        &mut self,
        id: TrackId,
        error: &QueueError,
        asks_again: bool,
        wanted: bool,
    ) -> bool {
        let Some(record) = self.find(id) else {
            return false;
        };
        if !matches!(record.status, TrackStatus::Loading | TrackStatus::Slow) {
            return false;
        }
        if record.load_cancelled() {
            self.set_status(id, TrackStatus::Cancelled);
            return false;
        }
        if asks_again && wanted {
            debug!(?id, %error, "a wanted load failed on a cause a later ask can answer; asking again");
            return true;
        }
        self.fail(id, error);
        false
    }

    /// Fail `id` with `error`. Its load gives the track's token up rather
    /// than cancelling it: a cover read beside the load still lands.
    pub(crate) fn fail(&mut self, id: TrackId, error: &QueueError) {
        if let Some(mut load) = self.record_mut(id).and_then(|record| record.load.take()) {
            load.disarm();
        }
        self.set_status(id, TrackStatus::Failed(error.to_string()));
    }

    fn find(&self, id: TrackId) -> Option<&TrackRecord<S>> {
        self.records.iter().find(|record| record.id == id)
    }

    /// Place the cover a load read for `id` and record
    /// [`QueueEvent::TrackMetadataChanged`], unless that load's token `load`
    /// is cancelled: a superseded load's cover is dropped.
    pub(crate) fn place_cover(&mut self, id: TrackId, load: &CancelToken, cover: Arc<Vec<u8>>) {
        if load.is_cancelled() {
            return;
        }
        let Some(record) = self.record_mut(id) else {
            return;
        };
        record.metadata.artwork = Some(cover);
        self.events.push(QueueEvent::TrackMetadataChanged { id });
    }

    fn record_mut(&mut self, id: TrackId) -> Option<&mut TrackRecord<S>> {
        self.records_mut().iter_mut().find(|record| record.id == id)
    }

    /// The records for a direct edit. Callers that only need to flip status
    /// should prefer [`Self::set_status`].
    pub(crate) fn records_mut(&mut self) -> &mut Vec<TrackRecord<S>> {
        self.revision += 1;
        &mut self.records
    }

    /// Take the changes recorded since the last take, in order, for the
    /// queue to announce.
    pub(crate) fn drain_events(&mut self) -> vec::Drain<'_, QueueEvent> {
        self.events.drain(..)
    }

    /// The rows the queue publishes for its handles, in queue order.
    pub(crate) fn rows(&self) -> Arc<[TrackRow<S>]> {
        self.records
            .iter()
            .map(|record| TrackRow {
                entry: record.entry(),
                source: record.source.clone(),
                observer: record.observer.clone(),
            })
            .collect()
    }

    /// Set `record.status` and record [`QueueEvent::TrackStatusChanged`].
    /// `Cancelled` also aborts the track's live load: a cancelled track never
    /// keeps loading.
    /// No-op when `id` is not present (caller raced `Queue::remove`).
    pub(crate) fn set_status(&mut self, id: TrackId, status: TrackStatus) {
        let Some(record) = self.record_mut(id) else {
            return;
        };
        record.status = status.clone();
        let aborted = matches!(status, TrackStatus::Cancelled)
            .then(|| record.load.take())
            .flatten();
        drop(aborted);
        self.events
            .push(QueueEvent::TrackStatusChanged { id, status });
    }

    /// Cancel every live load: the queue closed, and nothing it loads is
    /// wanted any more.
    pub(crate) fn cancel_loads(&mut self) {
        let loading: Vec<TrackId> = self
            .records
            .iter()
            .filter(|record| record.load.is_some())
            .map(|record| record.id)
            .collect();
        for id in loading {
            self.set_status(id, TrackStatus::Cancelled);
        }
    }
}
#[cfg(test)]
mod tests {
    use kithara_assets::AssetStore;
    use kithara_audio::{AudioObserveError, AudioObserver};
    use kithara_platform::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use kithara_signal::{AudioChunk, AudioChunkInfo};
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        loading::tests::config,
        test_pools::{TestPools, pools, sample_buffer},
    };

    #[kithara::test]
    #[case::from_str("https://example.com/song.mp3")]
    #[case::from_string("https://example.com/track.m3u8")]
    fn track_source_from_string_kind(#[case] url: &str) {
        let owned = url.to_string();
        let from_owned: TrackSource<TestPools> = owned.into();
        assert_eq!(from_owned.uri(), Some(url));
        let from_ref: TrackSource<TestPools> = url.into();
        assert_eq!(from_ref.uri(), Some(url));
    }

    #[kithara::test]
    fn track_source_from_resource_config() {
        let src =
            ResourceSrc::parse("https://example.com/a.mp3").expect("BUG: hard-coded URL is valid");
        let cfg = ResourceConfig::for_src(src)
            .store(AssetStore::builder(pools()).build())
            .build();
        let src: TrackSource<TestPools> = cfg.into();
        assert!(matches!(src, TrackSource::Config(_)));
        assert_eq!(src.uri(), Some("https://example.com/a.mp3"));
    }

    /// The first track's status.
    fn status(tracks: &Tracks<TestPools>) -> TrackStatus {
        tracks.records()[0].status.clone()
    }

    fn refusal(reason: &str) -> QueueError {
        QueueError::Resource(reason.to_owned())
    }

    /// Begin a load for `id` and return the track's token.
    fn begin(tracks: &mut Tracks<TestPools>, id: TrackId) -> CancelToken {
        let token = CancelToken::never().child();
        let load = TrackLoad::new(&config("https://x/a.mp3", token.clone()))
            .expect("the config carries its token");
        tracks.begin_load(id, load);
        token
    }

    fn tracks_with(id: TrackId) -> Tracks<TestPools> {
        let mut tracks = Tracks::default();
        tracks.records_mut().push(TrackRecord::new(
            id,
            String::new(),
            "https://x/a.mp3".into(),
        ));
        tracks
    }

    /// Two queued tracks, each carrying its own source, so a lookup that
    /// reaches the wrong record is visible rather than indistinguishable.
    fn two_tracks() -> Tracks<TestPools> {
        let mut tracks = Tracks::default();
        let records = tracks.records_mut();
        records.push(TrackRecord::new(
            TrackId(1),
            String::new(),
            "https://x/first.mp3".into(),
        ));
        records.push(TrackRecord::new(
            TrackId(2),
            String::new(),
            "https://x/second.mp3".into(),
        ));
        tracks
    }

    /// The slot a load of `id` hands its decoder.
    fn slot(tracks: &Tracks<TestPools>, id: TrackId) -> &AudioObserverSlot {
        &tracks.find(id).expect("the track is queued").observer
    }

    struct CountingObserver(Arc<AtomicUsize>);

    impl AudioObserver for CountingObserver {
        fn try_observe(&mut self, _chunk: &AudioChunk) -> Result<(), AudioObserveError> {
            self.0.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }

    #[kithara::test]
    fn an_attached_observer_reaches_its_own_tracks_decoder() {
        let pools = pools();
        let tracks = two_tracks();
        let seen = Arc::new(AtomicUsize::new(0));
        tracks.attach_observer(TrackId(2), Box::new(CountingObserver(Arc::clone(&seen))));
        let mut relay = slot(&tracks, TrackId(2)).relay();

        let chunk = AudioChunk::new(AudioChunkInfo::default(), sample_buffer(&pools, &[]));
        relay.try_observe(&chunk).expect("the observer accepts it");

        assert_eq!(seen.load(Ordering::Relaxed), 1);
    }

    #[kithara::test]
    fn an_attached_observer_does_not_reach_another_track() {
        let pools = pools();
        let tracks = two_tracks();
        let seen = Arc::new(AtomicUsize::new(0));
        tracks.attach_observer(TrackId(2), Box::new(CountingObserver(Arc::clone(&seen))));
        let mut relay = slot(&tracks, TrackId(1)).relay();

        let chunk = AudioChunk::new(AudioChunkInfo::default(), sample_buffer(&pools, &[]));
        relay
            .try_observe(&chunk)
            .expect("an empty relay is a no-op");

        assert_eq!(seen.load(Ordering::Relaxed), 0);
    }

    #[kithara::test]
    fn a_track_reports_its_own_source() {
        let tracks = two_tracks();

        assert_eq!(
            tracks
                .source(TrackId(2))
                .as_ref()
                .and_then(TrackSource::uri),
            Some("https://x/second.mp3")
        );
    }

    #[kithara::test]
    fn an_unqueued_track_has_no_source() {
        let tracks = two_tracks();

        assert!(tracks.source(TrackId(3)).is_none());
    }

    /// Nobody waits for the load the network refused for now: its track fails
    /// with the refusal, and the load is not asked again.
    #[kithara::test]
    fn an_unwanted_load_refused_for_now_fails_its_track() {
        let mut tracks = tracks_with(TrackId(1));
        begin(&mut tracks, TrackId(1));

        assert!(!tracks.refused(TrackId(1), &refusal("connection refused"), true, false));

        assert_eq!(
            status(&tracks),
            TrackStatus::Failed(refusal("connection refused").to_string())
        );
    }

    /// The queue still wants the track the network refused for now, so the
    /// track keeps loading and the queue asks again.
    #[kithara::test]
    fn a_wanted_load_refused_for_now_is_asked_again() {
        let mut tracks = tracks_with(TrackId(1));
        let token = begin(&mut tracks, TrackId(1));

        assert!(tracks.refused(TrackId(1), &refusal("connection refused"), true, true));

        assert_eq!(status(&tracks), TrackStatus::Loading);
        assert!(!token.is_cancelled(), "the wanted load was cut off");
    }

    /// A fresh load replaces the one in flight, which is cancelled.
    #[kithara::test]
    fn a_fresh_load_cancels_the_one_it_replaces() {
        let mut tracks = tracks_with(TrackId(1));
        let first = begin(&mut tracks, TrackId(1));
        let second = begin(&mut tracks, TrackId(1));

        assert!(first.is_cancelled());
        assert!(!second.is_cancelled());
        assert_eq!(status(&tracks), TrackStatus::Loading);
    }

    /// A track already standing in its slot has nothing left to load: a
    /// refusal that arrives for it changes nothing.
    #[kithara::test]
    fn a_loaded_track_is_not_failed_by_a_late_refusal() {
        let mut tracks = tracks_with(TrackId(1));
        begin(&mut tracks, TrackId(1));
        assert!(tracks.loaded(TrackId(1), &TrackMetadata::default()));

        tracks.refused(TrackId(1), &refusal("HTTP 404"), false, true);

        assert!(matches!(tracks.records()[0].status, TrackStatus::Loaded));
    }

    /// A load that opened hands its token to the resource and leaves the
    /// record free for the next load.
    #[kithara::test]
    fn an_opened_load_disarms_and_leaves_its_record_vacant() {
        let mut tracks = tracks_with(TrackId(1));
        let token = begin(&mut tracks, TrackId(1));

        assert!(tracks.loaded(TrackId(1), &TrackMetadata::default()));

        assert!(!token.is_cancelled(), "the resource's token was cancelled");
        assert!(tracks.records()[0].load.is_none());
    }

    /// A load fills the metadata its caller left unset from the decoder's
    /// tags, and keeps what the caller set.
    #[kithara::test]
    fn an_opened_load_fills_only_the_unset_metadata() {
        let mut tracks = tracks_with(TrackId(1));
        tracks.records_mut()[0].metadata.title = Some("caller".to_owned());
        begin(&mut tracks, TrackId(1));

        let tags = TrackMetadata {
            title: Some("decoder".to_owned()),
            artist: Some("decoder".to_owned()),
            ..TrackMetadata::default()
        };
        tracks.loaded(TrackId(1), &tags);

        let metadata = &tracks.records()[0].metadata;
        assert_eq!(metadata.title.as_deref(), Some("caller"));
        assert_eq!(metadata.artist.as_deref(), Some("decoder"));
    }

    #[kithara::test]
    fn removing_a_record_cancels_its_load() {
        let mut tracks = tracks_with(TrackId(1));
        let token = begin(&mut tracks, TrackId(1));
        tracks.records_mut().clear();
        assert!(token.is_cancelled(), "dropping the record aborts the load");
    }

    /// The caller's cancel reached the load's token while its open's answer
    /// was on its way to the queue. The load is over, and its track says so
    /// instead of loading forever.
    #[kithara::test]
    fn a_load_ended_by_its_cancel_leaves_its_track_cancelled() {
        let mut tracks = tracks_with(TrackId(1));
        let token = begin(&mut tracks, TrackId(1));

        token.cancel();
        assert!(!tracks.refused(TrackId(1), &refusal("cancelled"), true, true));

        assert_eq!(status(&tracks), TrackStatus::Cancelled);
    }

    /// A track the load opened before the caller cancelled it, and that the
    /// queue settles only afterwards, is not admitted: the cancel is the last
    /// word on that load.
    #[kithara::test]
    fn a_track_whose_load_was_cancelled_is_not_admitted() {
        let mut tracks = tracks_with(TrackId(1));
        let token = begin(&mut tracks, TrackId(1));

        token.cancel();

        assert!(
            !tracks.loaded(TrackId(1), &TrackMetadata::default()),
            "a cancelled load's track was admitted"
        );
        assert_eq!(status(&tracks), TrackStatus::Cancelled);
    }

    #[kithara::test]
    fn a_failed_load_fails_its_track() {
        let mut tracks = tracks_with(TrackId(1));
        begin(&mut tracks, TrackId(1));
        tracks.refused(TrackId(1), &refusal("boom"), false, true);
        assert_eq!(
            status(&tracks),
            TrackStatus::Failed(refusal("boom").to_string())
        );
    }

    /// A slow transfer is news only while its load runs: a report from a
    /// load that ended since changes nothing.
    #[kithara::test]
    fn only_a_running_load_turns_its_track_slow() {
        let mut tracks = tracks_with(TrackId(1));
        begin(&mut tracks, TrackId(1));
        let watch = |tracks: &Tracks<TestPools>| {
            tracks.records()[0]
                .load
                .as_ref()
                .map(|load| load.watch().clone())
                .expect("the track loads")
        };
        let ended = watch(&tracks);
        tracks.set_status(TrackId(1), TrackStatus::Cancelled);
        begin(&mut tracks, TrackId(1));

        tracks.apply_report(LoadReport::Slow {
            id: TrackId(1),
            watch: ended,
        });
        assert_eq!(status(&tracks), TrackStatus::Loading);

        let running = watch(&tracks);
        tracks.apply_report(LoadReport::Slow {
            id: TrackId(1),
            watch: running,
        });
        assert_eq!(status(&tracks), TrackStatus::Slow);
    }

    #[kithara::test]
    fn cancelling_loads_cancels_every_live_load() {
        let mut tracks = two_tracks();
        let first = begin(&mut tracks, TrackId(1));
        let second = begin(&mut tracks, TrackId(2));

        tracks.cancel_loads();

        assert!(first.is_cancelled() && second.is_cancelled());
        assert!(
            tracks
                .records()
                .iter()
                .all(|record| record.status == TrackStatus::Cancelled)
        );
    }
}
