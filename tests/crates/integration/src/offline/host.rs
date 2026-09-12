use std::{num::NonZeroU32, ops::Deref};

use kithara::{
    bufpool::{HasPool, PoolRegion},
    host::{Host, HostConfig, HostLevel, HostOwned},
    output::{OfflineRenderRequest, OfflineRenderer, OutputGroup, RenderSink, RenderSinkError},
    platform::{
        CancelScope,
        maybe_send::MaybeSend,
        sync::{
            Arc,
            atomic::{AtomicU64, Ordering},
        },
        time::Duration,
    },
    play::{MixTapWriter, PlayError, TransportRevision, player::PlayerControlSource},
    queue::Queue,
    signal::AudioSpec,
};
use ringbuf::{
    HeapCons, HeapRb,
    traits::{Consumer, Observer, Split},
};

use super::owner::HostOwner;

const CHANNELS: u16 = 2;
/// Cadence a device-free harness renders itself at when the test is not
/// pulling the playhead — the audio-device tick an offline session has no
/// device to receive.
pub const RENDER_PACE: Duration = Duration::from_millis(10);
/// Slack a playhead-against-cursor comparison needs. The product publishes
/// `PlaybackProgress` only once the reported position has moved
/// `PROGRESS_EMIT_MIN_DELTA_MS`, so an endpoint sourced from an event sits
/// that far from the cursor snapshot taken beside it — a lag that cancels
/// across a window whose two endpoints carry the same one, and consumes this
/// entire budget across a window whose endpoints do not.
const PROGRESS_QUANTUM_SECS: f64 = 0.1;

pub(super) const fn offline_pools<S>(config: &HostConfig<S>) -> &PoolRegion<S> {
    match config {
        HostConfig::Offline { pools, .. } => pools,
        _ => panic!("BUG: offline harness requires offline Host config"),
    }
}

struct HostState<S> {
    host: Host<S>,
    position: Arc<AtomicU64>,
}

/// Test owner for the product offline Host and its monotonic render cursor.
pub struct OfflineHostHarness<S> {
    off: HostOwner<HostState<S>>,
    position: Arc<AtomicU64>,
    spec: AudioSpec,
    max_block_frames: NonZeroU32,
}

/// Product Host plus the typed control for one resident test facade.
pub struct OfflineResident<P, S>
where
    P: PlayerControlSource<Schema = S>,
{
    host: OfflineHostHarness<S>,
    member: HostOwned<P>,
}

impl<P, S> OfflineResident<P, S>
where
    P: PlayerControlSource<Schema = S> + MaybeSend + 'static,
    P::Control: MaybeSend,
    S: HasPool<f32> + Send + Sync + 'static,
{
    pub async fn new(config: HostConfig<S>, player: P) -> Result<Self, PlayError> {
        Self::open(OfflineHostHarness::new(config).await?, player).await
    }

    /// Like [`Self::new`], with the Host rendering itself at `interval` so the
    /// playhead advances while the test waits on state rather than on renders.
    pub async fn paced(
        config: HostConfig<S>,
        player: P,
        interval: Duration,
    ) -> Result<Self, PlayError> {
        Self::open(OfflineHostHarness::paced(config, interval).await?, player).await
    }

    async fn open(host: OfflineHostHarness<S>, player: P) -> Result<Self, PlayError> {
        let member = host.insert(player).await?;
        Ok(Self { host, member })
    }

    pub async fn render(&self, frames: usize) -> Vec<f32> {
        self.host.render(frames).await
    }

    pub fn control(&self) -> P::Control {
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

    pub const fn host(&self) -> &OfflineHostHarness<S> {
        &self.host
    }

    /// Drops the resident before waiting for Host session teardown.
    pub async fn close(self) {
        let Self { host, member } = self;
        drop(member);
        host.close().await;
    }
}

impl<P, S> Deref for OfflineResident<P, S>
where
    P: PlayerControlSource<Schema = S>,
{
    type Target = P::Control;

    fn deref(&self) -> &Self::Target {
        self.member.control()
    }
}

pub type OfflineQueue<S> = OfflineResident<Queue<S>, S>;

impl<S> OfflineHostHarness<S>
where
    S: HasPool<f32> + Send + Sync + 'static,
{
    /// Build the same offline Host used by product rendering. The playhead
    /// then moves only where the test renders.
    pub async fn new(config: HostConfig<S>) -> Result<Self, PlayError> {
        Self::open(config, None).await
    }

    /// Like [`Self::new`], plus one render block per `interval` the owner
    /// thread spends idle. This is the audio-device tick an offline session
    /// has no device to receive: it lets a test wait on playback state the way
    /// an app does, instead of pulling every block itself.
    pub async fn paced(config: HostConfig<S>, interval: Duration) -> Result<Self, PlayError> {
        Self::open(config, Some(interval)).await
    }

    async fn open(config: HostConfig<S>, pacing: Option<Duration>) -> Result<Self, PlayError> {
        let spec = AudioSpec::new(CHANNELS, config.sample_rate());
        let max_block_frames = config
            .max_block_frames()
            .expect("offline Host config must have a render block size");
        let block = u64::from(max_block_frames.get());
        let position = Arc::new(AtomicU64::new(0));
        let owned = Arc::clone(&position);
        let start = move || {
            Host::new(config).map(|host| HostState {
                host,
                position: owned,
            })
        };
        let off = match pacing {
            None => HostOwner::spawn("offline-host", start).await?,
            Some(interval) => {
                HostOwner::spawn_paced("offline-host", start, interval, move |state| {
                    render_forward_on(state, spec, block, block);
                })
                .await?
            }
        };
        Ok(Self {
            off,
            position,
            spec,
            max_block_frames,
        })
    }

    /// Waits until the owner drops Host and session teardown completes.
    pub async fn close(self) {
        self.off.close().await;
    }

    /// Runs an arbitrary Host operation on the owner thread.
    pub async fn with<R>(&self, f: impl FnOnce(&mut Host<S>) -> R + MaybeSend + 'static) -> R
    where
        R: MaybeSend + 'static,
    {
        self.off.call(move |state| f(&mut state.host)).await
    }

    /// Runs a control call on the owner thread, the way product callers issue
    /// it from the app thread rather than from a runtime worker.
    pub async fn run<R>(&self, f: impl FnOnce() -> R + MaybeSend + 'static) -> R
    where
        R: MaybeSend + 'static,
    {
        self.off.call(move |_| f()).await
    }

    /// Transfer one configured player facade into the product Host.
    pub async fn insert<P>(&self, player: P) -> Result<HostOwned<P>, PlayError>
    where
        P: PlayerControlSource<Schema = S> + MaybeSend + 'static,
        P::Control: MaybeSend,
    {
        self.off.call(move |state| state.host.insert(player)).await
    }

    pub async fn insert_control<P>(&self, player: P) -> Result<P::Control, PlayError>
    where
        P: PlayerControlSource<Schema = S> + MaybeSend + 'static,
        P::Control: MaybeSend,
    {
        self.insert(player)
            .await
            .map(|owned| owned.control().clone())
    }

    /// Render the next finite block through the product offline protocol.
    pub async fn render(&self, frames: usize) -> Vec<f32> {
        let frames = u64::try_from(frames).expect("offline render frame count fits u64");
        let spec = self.spec;
        self.off
            .call(move |state| {
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
            })
            .await
    }

    /// Render `frames` forward from the renderer's own cursor through the
    /// product offline protocol, at the speed the decoder sustains. Returns
    /// the frames the timeline advanced.
    pub async fn render_forward(&self, frames: u64) -> u64 {
        let spec = self.spec;
        let block = u64::from(self.max_block_frames.get());
        self.off
            .call(move |state| render_forward_on(state, spec, block, frames))
            .await
    }

    /// Current finite-render cursor maintained by this harness.
    #[must_use]
    pub fn position(&self) -> u64 {
        self.position.load(Ordering::Relaxed)
    }

    /// Product offline output format.
    #[must_use]
    pub const fn spec(&self) -> AudioSpec {
        self.spec
    }

    /// Configured product render quantum.
    #[must_use]
    pub const fn max_block_frames(&self) -> NonZeroU32 {
        self.max_block_frames
    }

    pub async fn enable_mix_tap(&self, capacity: usize) -> Result<MixTapProbe, PlayError> {
        let (pcm_tx, pcm_rx) = HeapRb::<f32>::new(capacity).split();
        let drops = Arc::new(AtomicU64::new(0));
        self.install_mix_tap(MixTapWriter::new(pcm_tx, Arc::clone(&drops)))
            .await?;
        Ok(MixTapProbe { drops, pcm: pcm_rx })
    }

    pub async fn install_mix_tap(&self, writer: MixTapWriter) -> Result<(), PlayError> {
        let mut outputs = OutputGroup::new();
        outputs.push(writer);
        self.enable_outputs(outputs).await
    }

    pub async fn disable_mix_tap(&self) -> Result<(), PlayError> {
        self.off.call(|state| state.host.disable_outputs()).await
    }

    pub async fn enable_outputs(&self, outputs: OutputGroup) -> Result<(), PlayError> {
        self.off
            .call(move |state| state.host.enable_outputs(outputs))
            .await
    }

    pub async fn update_audio_route(&self, sample_rate: NonZeroU32) -> Result<(), PlayError> {
        self.off
            .call(move |state| state.host.update_audio_route(sample_rate))
            .await
    }

    pub async fn apply_mix<I>(&self, levels: I) -> Result<(), PlayError>
    where
        I: IntoIterator<Item = HostLevel>,
    {
        let levels: Vec<HostLevel> = levels.into_iter().collect();
        self.off
            .call(move |state| state.host.apply_mix(levels))
            .await
    }

    pub async fn transport_revision(&self) -> Result<TransportRevision, PlayError> {
        self.off
            .call(|state| {
                state
                    .host
                    .session_transport()
                    .map(|snapshot| snapshot.revision())
            })
            .await
    }

    pub async fn invalidate_audio_route(&self, reason: impl Into<String>) -> Result<(), PlayError> {
        let reason = reason.into();
        self.off
            .call(move |state| state.host.invalidate_audio_route(reason))
            .await
    }
}

/// Renders `frames` forward from the cursor the owner thread keeps, in `block`
/// quanta. Every render of this session runs on that thread, so the session's
/// own cursor and this one never disagree and a request never needs re-anchoring.
fn render_forward_on<S>(state: &mut HostState<S>, spec: AudioSpec, block: u64, frames: u64) -> u64
where
    S: HasPool<f32> + Send + Sync + 'static,
{
    let cancel = CancelScope::new(None);
    let mut cursor = state.position.load(Ordering::Relaxed);
    let mut rendered = 0;
    while rendered < frames {
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

pub struct MixTapProbe {
    drops: Arc<AtomicU64>,
    pcm: HeapCons<f32>,
}

impl MixTapProbe {
    pub fn drain(&mut self) -> Vec<f32> {
        self.pcm.pop_iter().collect()
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
/// the frames the renderer put through it. Read both endpoints the same way —
/// the same freshness of position, an [`OfflineHostHarness::position`] read
/// beside each — or the difference of the two reporting lags spends the slack
/// below before playback ever gets to.
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
