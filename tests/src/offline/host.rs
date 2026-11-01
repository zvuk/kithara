#[cfg(not(target_arch = "wasm32"))]
use std::num::NonZeroU64;
use std::{num::NonZeroU32, ops::Deref};

#[cfg(not(target_arch = "wasm32"))]
use kithara::play::{SessionError, TransportRevision};
use kithara::{
    bufpool::{HasPool, PoolRegion},
    host::{
        DeckControl, Host, HostConfig, HostCore, HostOwned, HostOwner as Owner,
        HostSettingsControl, Tap,
    },
    output::{OfflineRenderRequest, OfflineRenderer, OutputGroup, RenderSink, RenderSinkError},
    platform::{
        CancelScope,
        maybe_send::MaybeSend,
        sync::{
            Arc, Mutex,
            atomic::{AtomicU64, Ordering},
        },
        time::{Duration, Instant},
        tokio::sync::{broadcast, watch},
    },
    play::{DeckPass, HostedDeck, MixTapWriter, Outbox, PlayError, PlayWorker, TrackReceipt},
    queue::Queue,
    signal::AudioSpec,
    warp::{BeatGrid, BeatGridSnapshot},
};
use kithara_command::Refused;
use kithara_render::{
    bridge::{DeckEvent, DeckSnapshot, RtMetricsSnapshot},
    rt::DeckMixerConfig,
};
use ringbuf::{
    HeapCons, HeapRb,
    traits::{Consumer, Observer, Split},
};

use super::owner::HostOwner;
pub use super::render_clock::RenderClock;

pub struct ObservedDeck<P: DeckControl> {
    pub(super) inner: P,
    pub(super) snapshot: Arc<Mutex<DeckSnapshot>>,
    pub(super) underruns: Option<broadcast::Sender<DeckEvent>>,
}

impl<P: DeckControl> DeckControl for ObservedDeck<P> {
    type Control = P::Control;

    fn control(&self) -> Self::Control {
        self.inner.control()
    }
}

impl<P, S> HostedDeck<S> for ObservedDeck<P>
where
    P: DeckControl + HostedDeck<S>,
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    fn drain(&mut self, pass: DeckPass<'_>, out: &mut Outbox<'_, S>) {
        *self.snapshot.lock() = pass.deck.clone();
        self.inner.drain(pass, out);
    }

    fn settle(
        &mut self,
        receipt: TrackReceipt<'_, S>,
        pass: DeckPass<'_>,
        out: &mut Outbox<'_, S>,
    ) {
        *self.snapshot.lock() = pass.deck.clone();
        if let Some(underruns) = &self.underruns
            && let TrackReceipt::Event(event) = &receipt
            && matches!(
                event,
                DeckEvent::UnderrunStarted { .. } | DeckEvent::UnderrunEnded { .. }
            )
        {
            // No subscriber means no observation was requested; lag is checked by receivers.
            let _ = underruns.send(*event);
        }
        self.inner.settle(receipt, pass, out);
    }

    fn tick(&mut self, pass: DeckPass<'_>, out: &mut Outbox<'_, S>) {
        *self.snapshot.lock() = pass.deck.clone();
        self.inner.tick(pass, out);
    }

    delegate::delegate! {
        to self.inner {
            fn worker(&self) -> Option<&PlayWorker<S>>;
            fn mixer_config(&self) -> DeckMixerConfig;
            fn close(&mut self, out: &mut Outbox<'_, S>) -> Result<(), PlayError>;
            fn hold(&mut self, waker: std::task::Waker);
            fn release(&mut self);
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
use crate::usdt_trace;

const CHANNELS: u16 = 2;
/// Cadence a device-free harness renders itself at when the test is not
/// pulling the playhead — the audio-device tick an offline session has no
/// device to receive.
pub const RENDER_PACE: Duration = Duration::from_millis(10);

/// Audio-device cadence for the block geometry carried by `config`.
#[must_use]
pub fn audio_clock_pace<S>(config: &HostConfig<S>) -> Duration {
    let frames = config
        .max_block_frames()
        .expect("offline Host config must have a render block size");
    Duration::from_secs_f64(
        f64::from(frames.get()) / f64::from(config.settings().sample_rate().get()),
    )
}
/// Progress publication quantum. Sampling the latest committed event between
/// render blocks bounds each endpoint's reporting lag by this quantum.
const PROGRESS_QUANTUM_SECS: f64 = 0.1;

pub(super) const fn offline_pools<S>(config: &HostConfig<S>) -> &PoolRegion<S> {
    match config {
        HostConfig::Offline { pools, .. } => pools,
        _ => panic!("BUG: offline harness requires offline Host config"),
    }
}

struct HostState<S, O: Owner<S>>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    host: Host<S, RenderClock<O>>,
    position: Arc<AtomicU64>,
    render_started: watch::Receiver<Option<(u64, Instant)>>,
}

/// Test owner for the product offline Host and its monotonic render cursor.
pub struct OfflineHostHarness<S, O: Owner<S> = HostCore<S>>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    off: HostOwner<HostState<S, O>>,
    position: Arc<AtomicU64>,
    max_block_frames: NonZeroU32,
}

/// Product Host plus the typed control for one resident test facade.
pub struct OfflineResident<P, S>
where
    P: DeckControl,
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    host: OfflineHostHarness<S>,
    member: HostOwned<ObservedDeck<P>>,
    snapshot: Arc<Mutex<DeckSnapshot>>,
    underruns: broadcast::Sender<DeckEvent>,
}

impl<P, S> OfflineResident<P, S>
where
    P: DeckControl + HostedDeck<S> + MaybeSend + 'static,
    P::Control: MaybeSend,
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    pub async fn new(config: HostConfig<S>, player: P) -> Result<Self, PlayError> {
        Self::open(OfflineHostHarness::new(config).await?, player).await
    }

    /// Like [`Self::new`], with the Host rendering itself at `interval` so the
    /// playhead advances while the test waits on state rather than on renders.
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn paced(
        config: HostConfig<S>,
        player: P,
        interval: Duration,
    ) -> Result<Self, PlayError> {
        Self::open(OfflineHostHarness::paced(config, interval).await?, player).await
    }

    async fn open(host: OfflineHostHarness<S>, player: P) -> Result<Self, PlayError> {
        let (underruns, _) = broadcast::channel(kithara::events::DEFAULT_EVENT_BUS_CAPACITY);
        let (member, snapshot) = host
            .insert_observed_with_underruns(player, Some(underruns.clone()))
            .await?;
        Ok(Self {
            host,
            member,
            snapshot,
            underruns,
        })
    }

    pub async fn render(&self, frames: usize) -> Vec<f32> {
        self.host.render(frames).await
    }

    pub fn deck_snapshot(&self) -> DeckSnapshot {
        self.snapshot.lock().clone()
    }

    /// Observes complete mixer underrun edges before facade event translation.
    pub fn subscribe_underruns(&self) -> broadcast::Receiver<DeckEvent> {
        self.underruns.subscribe()
    }

    pub fn metrics(&self) -> RtMetricsSnapshot {
        self.snapshot.lock().metrics
    }

    pub fn control(&self) -> P::Control
    where
        P::Control: Clone,
    {
        self.member.control().clone()
    }

    /// Issues a control call from the host owner thread, as the app would.
    pub async fn run<R>(&self, f: impl FnOnce(&P::Control) -> R + MaybeSend + 'static) -> R
    where
        P::Control: Clone + MaybeSend + 'static,
        R: MaybeSend + 'static,
    {
        let control = self.control();
        self.host.run(move || f(&control)).await
    }

    delegate::delegate! {
        to self {
            #[field(&host)]
            pub const fn host(&self) -> &OfflineHostHarness<S>;
        }
    }

    /// Drops the resident before waiting for Host session teardown.
    #[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(5), ctx = String)]
    pub async fn close(self) {
        hang_tick!("resident close: dropping member before Host teardown".to_owned());
        let Self {
            host,
            member,
            snapshot,
            underruns,
        } = self;
        drop(underruns);
        drop(snapshot);
        drop(member);
        let _real_io = kithara::platform::flash::real_io();
        kithara::platform::time::timeout(__hang_detector.remaining(), host.close())
            .await
            .expect("offline resident teardown completes within its liveness budget");
        hang_tick!("resident close: Host teardown completed".to_owned());
        hang_reset!();
    }
}

/// Reads the control in place. A command goes through [`OfflineResident::run`]:
/// it waits for the control's owner, which an async test body must not.
impl<P, S> Deref for OfflineResident<P, S>
where
    P: DeckControl,
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    type Target = P::Control;

    fn deref(&self) -> &Self::Target {
        self.member.control()
    }
}

pub type OfflineQueue<S> = OfflineResident<Queue<S>, S>;

impl<S> OfflineHostHarness<S>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    /// Build the same offline Host used by product rendering. The playhead
    /// then moves only where the test renders.
    pub async fn new(config: HostConfig<S>) -> Result<Self, PlayError> {
        #[cfg(target_arch = "wasm32")]
        return Self::open(config).await;
        #[cfg(not(target_arch = "wasm32"))]
        Self::open(config, None).await
    }

    /// Like [`Self::new`], plus one render block per `interval` of the clock,
    /// however long a render or a call takes. This is the audio-device tick
    /// an offline session has no device to receive: it lets a test wait on
    /// playback state the way an app does, instead of pulling every block
    /// itself.
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn paced(config: HostConfig<S>, interval: Duration) -> Result<Self, PlayError> {
        Self::open(config, Some(interval)).await
    }

    async fn open(
        config: HostConfig<S>,
        #[cfg(not(target_arch = "wasm32"))] pacing: Option<Duration>,
    ) -> Result<Self, PlayError> {
        Self::layered(
            config,
            #[cfg(not(target_arch = "wasm32"))]
            pacing,
            |core| core,
        )
        .await
    }
}

impl<S, O> OfflineHostHarness<S, O>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
    O: Owner<S> + 'static,
{
    #[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(5), ctx = String)]
    pub async fn layered<L>(
        config: HostConfig<S>,
        #[cfg(not(target_arch = "wasm32"))] pacing: Option<Duration>,
        layer: L,
    ) -> Result<Self, PlayError>
    where
        L: FnOnce(HostCore<S, O::Deck>) -> O + MaybeSend + 'static,
    {
        let max_block_frames = config
            .max_block_frames()
            .expect("offline Host config must have a render block size");
        #[cfg(not(target_arch = "wasm32"))]
        let block = u64::from(max_block_frames.get());
        let position = Arc::new(AtomicU64::new(0));
        let owned = Arc::clone(&position);
        let (started, render_started) = watch::channel(None);
        let start = move || {
            Host::layered(config, move |core| RenderClock::new(layer(core), started)).map(|host| {
                HostState {
                    host,
                    position: owned,
                    render_started,
                }
            })
        };
        hang_tick!("layered: awaiting Host owner initialization".to_owned());
        let _real_io = kithara::platform::flash::real_io();
        let off = kithara::platform::time::timeout(__hang_detector.remaining(), async {
            #[cfg(target_arch = "wasm32")]
            let off = HostOwner::spawn("offline-host", start).await?;
            #[cfg(not(target_arch = "wasm32"))]
            let off = match pacing {
                None => HostOwner::spawn("offline-host", start).await?,
                Some(interval) => {
                    HostOwner::spawn_paced("offline-host", start, interval, move |state| {
                        render_forward_on(state, block, block);
                    })
                    .await?
                }
            };
            Ok::<_, PlayError>(off)
        })
        .await;
        hang_tick!("layered: awaiting Host owner initialization".to_owned());
        let off = off.expect("offline Host initializes within its liveness budget")?;
        hang_reset!();
        Ok(Self {
            off,
            position,
            max_block_frames,
        })
    }

    /// Waits until the owner drops Host and session teardown completes.
    #[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(5), ctx = String)]
    pub async fn close(self) {
        hang_tick!("Host close: awaiting owner and session teardown".to_owned());
        let _real_io = kithara::platform::flash::real_io();
        let completed =
            kithara::platform::time::timeout(__hang_detector.remaining(), self.off.close()).await;
        hang_tick!("Host close: checking owner teardown answer".to_owned());
        completed.expect("offline Host teardown completes within its liveness budget");
        hang_reset!();
    }

    #[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(5), ctx = String)]
    async fn call<R>(&self, job: impl FnOnce(&mut HostState<S, O>) -> R + MaybeSend + 'static) -> R
    where
        R: MaybeSend + 'static,
    {
        hang_tick!("Host call: awaiting owner-thread operation answer".to_owned());
        let answer =
            kithara::platform::time::timeout(__hang_detector.remaining(), self.off.call(job)).await;
        hang_tick!("Host call: checking owner-thread operation answer".to_owned());
        let answer = answer.expect("offline Host operation completes within its liveness budget");
        hang_reset!();
        answer
    }

    /// Runs an arbitrary Host operation on the owner thread.
    pub async fn with<R>(
        &self,
        f: impl FnOnce(&mut Host<S, RenderClock<O>>) -> R + MaybeSend + 'static,
    ) -> R
    where
        R: MaybeSend + 'static,
    {
        self.call(move |state| f(&mut state.host)).await
    }

    /// Posts a command and observes the canonical owner's answer.
    #[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(5), ctx = String)]
    pub async fn send(&self, command: O::Command) -> Result<(), PlayError>
    where
        O::Command: 'static,
    {
        hang_tick!("Host send: awaiting owner admission".to_owned());
        let remaining = __hang_detector.remaining();
        let answer = self
            .with(move |host| {
                host.send(command)?
                    .wait_until(Instant::now() + remaining)
                    .ok()
                    .expect("Host admission completes within its liveness budget")
                    .map_err(|refused| match refused {
                        Refused::Owner(error) => error,
                        Refused::Unanswered => PlayError::SessionGone {
                            reason: "the owner dropped a command unanswered",
                        },
                    })
            })
            .await;
        hang_reset!();
        answer
    }

    /// Runs a control call on the owner thread, the way product callers issue
    /// it from the app thread rather than from a runtime worker.
    pub async fn run<R>(&self, f: impl FnOnce() -> R + MaybeSend + 'static) -> R
    where
        R: MaybeSend + 'static,
    {
        self.call(move |_| f()).await
    }

    /// Samples an observation and the render cursor between completed blocks.
    /// The renderer cannot advance while `sample` reads its committed events.
    pub async fn observe<R>(&self, sample: impl FnOnce() -> R + MaybeSend + 'static) -> (R, u64)
    where
        R: MaybeSend + 'static,
    {
        self.call(move |state| (sample(), state.position.load(Ordering::Relaxed)))
            .await
    }

    /// Render the next finite block through the product offline protocol.
    pub async fn render(&self, frames: usize) -> Vec<f32> {
        self.call(move |state| render_on(state, frames)).await
    }

    /// Render one callback and return its engine-thread start time beside its samples.
    pub async fn render_timed(&self, frames: usize) -> (Instant, Vec<f32>) {
        assert!(frames > 0 && frames <= self.max_block_frames.get() as usize);
        self.call(move |state| {
            let start = state.position.load(Ordering::Relaxed);
            let samples = render_on(state, frames);
            let (position, started) = (*state.render_started.borrow())
                .expect("the engine observed the rendered callback");
            assert_eq!(
                position, start,
                "timestamp belongs to the requested callback"
            );
            (started, samples)
        })
        .await
    }

    /// Render `frames` forward from the renderer's own cursor through the
    /// product offline protocol, at the speed the decoder sustains. Returns
    /// the frames the timeline advanced.
    pub async fn render_forward(&self, frames: u64) -> u64 {
        let block = u64::from(self.max_block_frames.get());
        self.call(move |state| render_forward_on(state, block, frames))
            .await
    }

    /// Current finite-render cursor maintained by this harness.
    #[must_use]
    pub fn position(&self) -> u64 {
        self.position.load(Ordering::Relaxed)
    }

    /// Product offline output format at the rate the session renders now.
    pub async fn spec(&self) -> AudioSpec {
        self.call(|state| output_spec(&state.host)).await
    }

    /// Configured product render quantum.
    #[must_use]
    pub const fn max_block_frames(&self) -> NonZeroU32 {
        self.max_block_frames
    }

    pub async fn attach_tap(&self, tap: Tap, capacity: usize) -> Result<TapProbe, PlayError> {
        let (pcm_tx, pcm_rx) = HeapRb::<f32>::new(capacity).split();
        let drops = Arc::new(AtomicU64::new(0));
        let mut outputs = OutputGroup::new();
        outputs.push(MixTapWriter::new(pcm_tx, Arc::clone(&drops)));
        self.attach_outputs(tap, outputs).await?;
        Ok(TapProbe { drops, pcm: pcm_rx })
    }

    pub async fn attach_outputs(&self, tap: Tap, outputs: OutputGroup) -> Result<(), PlayError> {
        self.call(move |state| state.host.attach_outputs(tap, outputs))
            .await
    }

    pub async fn detach_tap(&self, tap: Tap) -> Result<(), PlayError> {
        self.call(move |state| state.host.detach_outputs(tap)).await
    }

    /// The callback transport revision after a renderer commit was recorded.
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn transport_revision(&self) -> Result<TransportRevision, PlayError> {
        usdt_trace::last("render_committed")
            .and_then(|event| event.field("transport_revision"))
            .and_then(NonZeroU64::new)
            .ok_or(PlayError::Session(SessionError::TransportNotProcessed))?;
        kithara_test_utils::test::usdt::events_of("publish")
            .into_iter()
            .rev()
            .find(|event| event.target == "kithara_warp_probe")
            .and_then(|event| event.field("transport_revision"))
            .and_then(NonZeroU64::new)
            .map(TransportRevision::from)
            .ok_or(PlayError::Session(SessionError::TransportNotProcessed))
    }

    pub async fn set_sample_rate(&self, sample_rate: NonZeroU32) -> Result<(), PlayError> {
        self.call(move |state| state.host.set_sample_rate(sample_rate))
            .await
    }

    /// The session grid the Host publishes as its own beat grid.
    pub async fn session_grid(&self) -> BeatGridSnapshot {
        self.call(|state| state.host.snapshot()).await
    }

    pub async fn invalidate_audio_route(&self, reason: impl Into<String>) -> Result<(), PlayError> {
        let reason = reason.into();
        self.call(move |state| state.host.invalidate_audio_route(reason))
            .await
    }
}

fn render_on<S, O: Owner<S>>(state: &mut HostState<S, O>, frames: usize) -> Vec<f32>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    let frames = u64::try_from(frames).expect("offline render frame count fits u64");
    let spec = output_spec(&state.host);
    let start = state.position.load(Ordering::Relaxed);
    let end = start
        .checked_add(frames)
        .expect("offline render timeline fits u64");
    let request = OfflineRenderRequest::builder()
        .spec(spec)
        .frames(start..end)
        .build();
    let cancel = CancelScope::new(None);
    let mut sink = VecSink::default();
    state
        .host
        .render(&request, &cancel.token(), &mut sink)
        .unwrap_or_else(|error| panic!("render product offline Host: {error}"));
    state.position.store(end, Ordering::Relaxed);
    sink.samples
}

/// Renders `frames` forward from the cursor the owner thread keeps, in `block`
/// quanta. Every render of this session runs on that thread, so the session's
/// own cursor and this one never disagree and a request never needs re-anchoring.
#[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(5), ctx = String)]
fn render_forward_on<S, O: Owner<S>>(state: &mut HostState<S, O>, block: u64, frames: u64) -> u64
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    let spec = output_spec(&state.host);
    let cancel = CancelScope::new(None);
    let mut cursor = state.position.load(Ordering::Relaxed);
    let mut rendered = 0;
    while rendered < frames {
        hang_tick!(format!(
            "Host render: {rendered}/{frames} frames complete, cursor {cursor}, block {block}"
        ));
        let end = cursor
            .checked_add(block.min(frames - rendered))
            .expect("offline render timeline fits u64");
        let request = OfflineRenderRequest::builder()
            .spec(spec)
            .frames(cursor..end)
            .build();
        let report = state
            .host
            .render(&request, &cancel.token(), &mut DiscardSink)
            .unwrap_or_else(|error| panic!("render product offline Host forward: {error}"));
        cursor = end;
        rendered += report.frames;
        if report.frames > 0 {
            hang_reset!();
        }
    }
    state.position.store(cursor, Ordering::Relaxed);
    rendered
}

/// Drops rendered audio: a forward render is taken for the timeline it
/// advances, not for the samples it produces.
struct DiscardSink;

impl RenderSink for DiscardSink {
    fn write(&mut self, _samples: &[f32]) -> Result<(), RenderSinkError> {
        Ok(())
    }
}

#[derive(Default)]
struct VecSink {
    samples: Vec<f32>,
}

impl RenderSink for VecSink {
    fn write(&mut self, samples: &[f32]) -> Result<(), RenderSinkError> {
        self.samples.extend_from_slice(samples);
        Ok(())
    }
}

pub struct TapProbe {
    drops: Arc<AtomicU64>,
    pcm: HeapCons<f32>,
}

impl TapProbe {
    pub fn drain(&mut self) -> Vec<f32> {
        self.pcm.pop_iter().collect()
    }

    /// Drains a delivered batch into preallocated storage, rejecting incomplete PCM.
    pub(crate) fn drain_exact_into(
        &mut self,
        samples: &mut Vec<f32>,
        frames: u64,
        channels: u16,
    ) -> Result<(), String> {
        let start = samples.len();
        samples.extend(self.pcm.pop_iter());
        let expected = usize::try_from(frames)
            .expect("batch frames fit usize")
            .checked_mul(usize::from(channels))
            .expect("batch samples fit usize");
        assert!(channels > 0, "capture channels must be non-zero");
        let drops = self.drops();
        if drops != 0 {
            return Err(format!(
                "Output tap lost samples: observed {drops}, expected 0"
            ));
        }
        let captured = samples.len() - start;
        if captured != expected {
            return Err(format!(
                "Output tap captures every delivered channel: captured {captured}, expected {expected}"
            ));
        }
        if samples[start..].iter().any(|sample| !sample.is_finite()) {
            return Err("non-finite Output PCM".to_owned());
        }
        Ok(())
    }

    #[must_use]
    pub fn drops(&self) -> u64 {
        self.drops.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn writer_alive(&self) -> bool {
        self.pcm.write_is_held()
    }
}

/// Asserts the position the player reported over one measurement window tracks
/// the frames the renderer put through it. Use [`OfflineHostHarness::observe`]
/// to sample the latest committed position alongside the cursor: an observer
/// can be descheduled between receiving an event and reading the cursor.
///
/// The two numbers are kept by different owners — the cursor by the offline
/// renderer, the position by the player — so their agreement is a property of
/// playback rather than a restatement of the render cadence, and it holds at
/// whatever cadence the harness renders at.
///
/// # Panics
///
/// Panics when the playhead and the cursor disagree by more than one progress
/// quantum, naming both numbers.
pub fn assert_playhead_tracks_renderer(gain: f64, frames: u64, spec: AudioSpec, label: &str) {
    let rendered = spec
        .duration_for(frames)
        .expect("render cursor advance fits a duration")
        .as_secs_f64();
    let drift = gain - rendered;
    assert!(
        drift.abs() <= PROGRESS_QUANTUM_SECS,
        "playhead lost the renderer [{label}]: position gained {gain:.3}s while the renderer \
         advanced {rendered:.3}s ({frames} frames), a drift of {drift:.3}s over the \
         {PROGRESS_QUANTUM_SECS}s progress quantum"
    );
}

/// The output format the session renders at now; a rate change moves it.
fn output_spec<S, O: Owner<S>>(host: &Host<S, O>) -> AudioSpec
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    let rate = host.output_sample_rate().output();
    AudioSpec::new(
        CHANNELS,
        NonZeroU32::new(rate).expect("product offline Host renders at a non-zero rate"),
    )
}

impl<S> OfflineHostHarness<S>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    /// Transfer one configured player facade into the product Host.
    pub async fn insert<P>(&self, player: P) -> Result<HostOwned<P>, PlayError>
    where
        P: DeckControl + HostedDeck<S> + MaybeSend + 'static,
        P::Control: MaybeSend,
    {
        self.call(move |state| state.host.insert(player)).await
    }

    pub async fn insert_observed<P>(
        &self,
        player: P,
    ) -> Result<(HostOwned<ObservedDeck<P>>, Arc<Mutex<DeckSnapshot>>), PlayError>
    where
        P: DeckControl + HostedDeck<S> + MaybeSend + 'static,
        P::Control: MaybeSend,
    {
        self.insert_observed_with_underruns(player, None).await
    }

    async fn insert_observed_with_underruns<P>(
        &self,
        player: P,
        underruns: Option<broadcast::Sender<DeckEvent>>,
    ) -> Result<(HostOwned<ObservedDeck<P>>, Arc<Mutex<DeckSnapshot>>), PlayError>
    where
        P: DeckControl + HostedDeck<S> + MaybeSend + 'static,
        P::Control: MaybeSend,
    {
        let snapshot = Arc::new(Mutex::new(DeckSnapshot::default()));
        let member = self
            .insert(ObservedDeck {
                inner: player,
                snapshot: snapshot.clone(),
                underruns,
            })
            .await?;
        Ok((member, snapshot))
    }

    pub async fn insert_control<P>(&self, player: P) -> Result<P::Control, PlayError>
    where
        P: DeckControl + HostedDeck<S> + MaybeSend + 'static,
        P::Control: Clone + MaybeSend,
    {
        self.insert(player)
            .await
            .map(|owned| owned.control().clone())
    }
}

#[cfg(test)]
mod capture_tests {
    use ringbuf::traits::Producer;

    use super::*;

    #[kithara_test_utils::kithara::test(native)]
    fn output_capture_preserves_all_channels_and_empty_tail() {
        let (mut writer, pcm) = HeapRb::<f32>::new(4).split();
        let input = [0.0_f32, -0.0, 0.25, -0.5];
        assert_eq!(writer.push_slice(&input), input.len());
        let mut tap = TapProbe {
            drops: Arc::new(AtomicU64::new(0)),
            pcm,
        };
        let mut captured = Vec::with_capacity(input.len());
        tap.drain_exact_into(&mut captured, 2, 2)
            .expect("complete Output PCM");
        tap.drain_exact_into(&mut captured, 0, 2)
            .expect("empty Output tail");
        assert_eq!(
            captured
                .iter()
                .map(|sample| sample.to_bits())
                .collect::<Vec<_>>(),
            input.map(f32::to_bits)
        );
        assert!(tap.writer_alive());
        drop(writer);
        assert!(!tap.writer_alive());
    }

    #[kithara_test_utils::kithara::test(native)]
    fn output_capture_rejects_missing_extra_dropped_and_nonfinite_samples() {
        for (input, drops, expected) in [
            (
                vec![0.0_f32],
                0,
                "Output tap captures every delivered channel: captured 1, expected 2",
            ),
            (
                vec![0.0, 0.0, 0.0],
                0,
                "Output tap captures every delivered channel: captured 3, expected 2",
            ),
            (
                vec![0.0, 0.0],
                1,
                "Output tap lost samples: observed 1, expected 0",
            ),
            (vec![0.0, f32::NAN], 0, "non-finite Output PCM"),
            (vec![f32::INFINITY, 0.0], 0, "non-finite Output PCM"),
        ] {
            let (mut writer, pcm) = HeapRb::<f32>::new(4).split();
            assert_eq!(writer.push_slice(&input), input.len());
            let mut tap = TapProbe {
                drops: Arc::new(AtomicU64::new(drops)),
                pcm,
            };
            assert_eq!(
                tap.drain_exact_into(&mut Vec::with_capacity(4), 1, 2)
                    .expect_err("invalid capture must not become a passing witness"),
                expected
            );
        }
    }
}
