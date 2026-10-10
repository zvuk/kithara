use std::{error::Error as StdError, io::Error};

use kithara_assets::AssetStore;
use kithara_audio::AudioObserver;
use kithara_bufpool::HasPool;
use kithara_download::DownloaderEvent;
use kithara_events::{Envelope, EventReceiver, RecvError, ScopeLabel, TrackId};
use kithara_net::NetError;
use kithara_platform::{
    CancelToken,
    sync::Arc,
    tokio,
    tokio::{runtime::Handle as RuntimeHandle, task::spawn_on},
};
use kithara_play::{
    ArtifactLoadError, Cover, LoadRefusal, OutputSnapshot, ResourceConfig, ResourceLoad,
    ResourcePrep, ResourceSrc,
};
use tracing::{debug, warn};

use crate::{
    error::QueueError,
    loading::{LoadReport, TrackLoad},
    queue::{QueueCommand, QueuePostbox},
    track::TrackSource,
};

/// What the queue opens each track with: its sources become prepared
/// configs, and the tasks beside a load report a slow transfer or a cover to
/// the queue's mailbox. The open itself goes through the track to the
/// dispatcher of the deck's owner.
pub(crate) struct Loader<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    prep: ResourcePrep<S>,
    store: AssetStore<S>,
    /// Where the tasks beside each load run; `None` fails every load.
    runtime: Option<RuntimeHandle>,
    /// Where a task beside a load reports to the queue.
    postbox: QueuePostbox<S>,
}

impl<S> Loader<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    pub(crate) fn new(
        prep: ResourcePrep<S>,
        store: AssetStore<S>,
        runtime: Option<RuntimeHandle>,
        postbox: QueuePostbox<S>,
    ) -> Self {
        Self {
            prep,
            store,
            runtime,
            postbox,
        }
    }
}

impl<S> Loader<S>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    /// Build a [`ResourceConfig`] for the given [`TrackSource`].
    ///
    /// - [`TrackSource::Uri`] uses the queue store; other resource options
    ///   keep their defaults. Callers wanting custom behavior build a
    ///   configured [`ResourceConfig`] and pass it via
    ///   [`TrackSource::Config`].
    /// - [`TrackSource::Config`] is passed through untouched (DRM keys,
    ///   headers, format hints preserved).
    ///
    /// Both paths finish with the deck's prepare, so the worker, the session
    /// output and the bus labelled with the track are injected.
    pub(crate) fn build_config(
        &self,
        id: TrackId,
        source: TrackSource<S>,
        output: &OutputSnapshot,
    ) -> Result<ResourceConfig<S>, QueueError> {
        let mut config = match source {
            TrackSource::Uri(url) => {
                let src = ResourceSrc::parse(&url)
                    .map_err(|e| QueueError::InvalidUrl(format!("{url}: {e}")))?;
                ResourceConfig::for_src(src)
                    .store(self.store.clone())
                    .build()
            }
            TrackSource::Config(boxed) => *boxed,
        };
        if config.bus().is_none() {
            config.set_bus(self.prep.bus.scoped_labeled(ScopeLabel {
                track: Some(id),
                ..ScopeLabel::default()
            }));
        }
        self.prep.prepare(config, output).map_err(QueueError::from)
    }

    /// The open of `id` from `source`, its decoder reaching `observer`, and
    /// the load that owns its token. The cover read and the slow-transfer
    /// watch start beside it.
    pub(crate) fn start(
        &self,
        id: TrackId,
        source: TrackSource<S>,
        observer: Box<dyn AudioObserver>,
        output: &OutputSnapshot,
    ) -> Result<(ResourceLoad<S>, TrackLoad), QueueError> {
        let config = self.build_config(id, source, output)?;
        let load = TrackLoad::new(&config)?;
        let runtime = self.runtime.as_ref().ok_or(QueueError::NoRuntime)?;
        self.read_cover(runtime, id, &config, &load);
        self.watch_for_slow_transfer(runtime, id, &config, &load);
        Ok((ResourceLoad::new(config, observer), load))
    }

    /// Read the track's cover beside its audio, over the load's transport and
    /// the track's token, and report it to the queue, which places it while
    /// that token lives. The audio never waits for the cover, and a cover
    /// that never arrives leaves the load untouched.
    fn read_cover(
        &self,
        runtime: &RuntimeHandle,
        id: TrackId,
        config: &ResourceConfig<S>,
        load: &TrackLoad,
    ) {
        let Some(cover) = config.artwork().cloned() else {
            return;
        };
        let config = config.clone();
        let token = load.token().clone();
        let postbox = self.postbox.clone();
        drop(spawn_on(runtime, async move {
            match config.artifact_fetch().load::<Cover>(&cover).await {
                Ok(cover) => report(
                    &postbox,
                    LoadReport::Cover {
                        id,
                        load: token,
                        cover: Arc::new(cover.into()),
                    },
                ),
                Err(ArtifactLoadError::Cancelled { .. }) => {}
                Err(error) => warn!(?id, %error, "the track's cover never arrived"),
            }
        }));
    }

    /// Watch the load's bus for its first slow transfer. The watch
    /// subscribes here, before the load is sent, so no `LoadSlow` slips past
    /// it, and ends with the load.
    fn watch_for_slow_transfer(
        &self,
        runtime: &RuntimeHandle,
        id: TrackId,
        config: &ResourceConfig<S>,
        load: &TrackLoad,
    ) {
        let Some(bus) = config.bus() else {
            return;
        };
        drop(spawn_on(
            runtime,
            slow_transfer(
                id,
                load.watch().clone(),
                bus.subscribe(),
                self.postbox.clone(),
            ),
        ));
    }
}

/// Post `report` to the queue. A queue that is gone has no track left to
/// report on.
fn report<S>(postbox: &QueuePostbox<S>, report: LoadReport)
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    if postbox.post(QueueCommand::Load(report)).is_err() {
        debug!("the queue is gone: dropping a load's report");
    }
}

/// Report the first [`DownloaderEvent::LoadSlow`] `events` delivers to the
/// queue, which turns the track [`TrackStatus::Slow`](crate::TrackStatus::Slow),
/// unless `watch` ends first. A `Lagged` bus dropped the oldest envelopes and
/// keeps delivering, so the watch survives the gap; only `Closed` ends it.
async fn slow_transfer<S>(
    id: TrackId,
    watch: CancelToken,
    mut events: EventReceiver<DownloaderEvent>,
    postbox: QueuePostbox<S>,
) where
    S: HasPool<u8> + Send + Sync + 'static,
{
    let slow = async {
        loop {
            match events.recv().await {
                Ok(Envelope {
                    event: DownloaderEvent::LoadSlow { .. },
                    ..
                }) => return true,
                Ok(_) | Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => return false,
            }
        }
    };
    tokio::select! {
        biased;
        () = watch.cancelled() => {}
        slow = slow => if slow {
            report(&postbox, LoadReport::Slow { id, watch: watch.clone() });
        },
    }
}

/// Whether the dispatcher's refusal of an open is worth asking for again as
/// it stands: only an open that failed on a network cause a later ask can
/// answer.
pub(crate) fn asks_again(refusal: &LoadRefusal) -> bool {
    match refusal {
        LoadRefusal::Open(error) => can_answer_later(error),
        LoadRefusal::Capacity { .. }
        | LoadRefusal::Cancelled
        | LoadRefusal::NoRuntime
        | LoadRefusal::Pool(_)
        | LoadRefusal::Source(_) => false,
    }
}

/// Retry only when the error's typed network cause can answer later.
/// Error chains, not messages or asynchronous bus events, provide the verdict.
/// Parser/codec failures and stalled transfers are never connectivity retries.
fn can_answer_later(error: &(dyn StdError + 'static)) -> bool {
    net_cause(error).is_some_and(NetError::can_answer_later)
}

/// The network failure behind a load error, if the load failed on the network at
/// all.
///
/// [`io::Error`] hides its payload from [`StdError::source`] — it reports the
/// payload's *own* source instead — so a plain chain walk steps straight over a
/// wrapped `NetError`. This looks inside one explicitly.
fn net_cause<'e>(error: &'e (dyn StdError + 'static)) -> Option<&'e NetError> {
    let mut current = Some(error);
    while let Some(err) = current {
        if let Some(net) = err.downcast_ref::<NetError>() {
            return Some(net);
        }
        if let Some(net) = err
            .downcast_ref::<Error>()
            .and_then(Error::get_ref)
            .and_then(|payload| net_cause(payload))
        {
            return Some(net);
        }
        current = err.source();
    }
    None
}
#[cfg(test)]
mod tests {
    use std::num::NonZeroU16;

    use kithara_test_utils::kithara;

    use super::*;

    /// A spent budget over a refusal keeps the resource askable: the load is
    /// repeated while the selection wants it, which is how a track chosen during
    /// an outage starts once connectivity returns.
    #[kithara::test]
    fn a_refused_host_can_answer_later() {
        let refused = NetError::RetryExhausted {
            max_retries: 3,
            source: Box::new(NetError::Status {
                status: NonZeroU16::new(503).expect("503 is not zero"),
                url: None,
                body: Some("network offline".to_string()),
            }),
        };
        // `io::Error` hides its payload from the source chain; the classifier looks inside.
        assert!(can_answer_later(&Error::other(refused)));
    }

    /// A vanished transport is the same answer: nothing was reached, so the whole
    /// load is worth asking for again.
    #[kithara::test]
    fn a_vanished_host_can_answer_later() {
        let gone = NetError::Network("connection closed".to_string());
        assert!(can_answer_later(&Error::other(gone)));
    }

    /// A transfer that established and then stopped delivering is the net layer's
    /// own verdict: repeating it would spin instead of telling the user, the
    /// contract `stalled_master_playlist_fails_load` pins.
    #[kithara::test]
    fn a_stalled_transfer_is_not_asked_again() {
        let stalled = NetError::RetryExhausted {
            max_retries: 1,
            source: Box::new(NetError::Timeout),
        };
        assert!(!can_answer_later(&Error::other(stalled)));
    }

    /// A missing resource answers the same however long one waits.
    #[kithara::test]
    fn a_missing_resource_is_not_asked_again() {
        let missing = NetError::Status {
            status: NonZeroU16::new(404).expect("404 is not zero"),
            url: None,
            body: None,
        };
        assert!(!can_answer_later(&Error::other(missing)));
    }

    /// A failure the network had no part in — an unparseable container, a codec
    /// the build does not carry — is not a connectivity question.
    #[kithara::test]
    fn a_failure_with_no_network_cause_is_not_asked_again() {
        let local = Error::other("unsupported container");
        assert!(!can_answer_later(&local));
    }

    /// A cancelled open is the track's own decision, not an outage.
    #[kithara::test]
    fn a_cancelled_open_is_not_asked_again() {
        assert!(!asks_again(&LoadRefusal::Cancelled));
    }

    #[kithara::test]
    fn a_terminal_source_failure_is_not_asked_again() {
        use kithara_audio::{DecodeErrorKind, TrackFailureKind};

        for failure in [
            TrackFailureKind::Decode {
                kind: DecodeErrorKind::Io,
            },
            TrackFailureKind::RecreateFailed { offset: 91 },
            TrackFailureKind::SourceCancelled,
            TrackFailureKind::ChannelClosed,
            TrackFailureKind::Render,
        ] {
            assert!(!asks_again(&LoadRefusal::Source(failure)));
        }
    }
}
