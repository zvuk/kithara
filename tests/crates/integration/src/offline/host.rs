use std::{
    num::NonZeroU32,
    ops::{Deref, RangeInclusive},
};

use kithara::{
    bufpool::{HasPool, PoolRegion},
    host::{Host, HostConfig, HostLevel, HostOwned, testing::HostProbe},
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
const ENDPOINT_SLACK_SECS: f64 = 0.5;
const GAIN_FLOOR_SECS: f64 = 0.9;

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
    pacing: Option<Duration>,
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
        let host = OfflineHostHarness::new(config).await?;
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
    /// Build the same offline Host used by product rendering.
    pub async fn new(config: HostConfig<S>) -> Result<Self, PlayError> {
        let spec = AudioSpec::new(CHANNELS, config.sample_rate());
        let max_block_frames = config
            .max_block_frames()
            .expect("offline Host config must have a render block size");
        let pacing = config.pacing();
        let position = Arc::new(AtomicU64::new(0));
        let owned = Arc::clone(&position);
        let off = HostOwner::spawn("offline-host", move || {
            Host::new(config).map(|host| HostState {
                host,
                position: owned,
            })
        })
        .await?;
        Ok(Self {
            off,
            position,
            spec,
            max_block_frames,
            pacing,
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

    /// Configured automatic test/probe cadence.
    #[must_use]
    pub const fn pacing(&self) -> Option<Duration> {
        self.pacing
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

    pub async fn restart_stream(&self, sample_rate: u32) -> Result<(), PlayError> {
        self.off
            .call(move |state| state.host.restart_stream(sample_rate))
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
        self.off.call(|state| state.host.transport_revision()).await
    }

    pub async fn invalidate_audio_route(&self, reason: impl Into<String>) -> Result<(), PlayError> {
        let reason = reason.into();
        self.off
            .call(move |state| state.host.invalidate_audio_route(reason))
            .await
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

/// Expected playback-position gain for one configured paced offline session.
#[must_use]
pub fn offline_gain_window(
    window_secs: f64,
    sample_rate: NonZeroU32,
    block_frames: NonZeroU32,
    pacing: Duration,
) -> RangeInclusive<f64> {
    let rate =
        (f64::from(block_frames.get()) / f64::from(sample_rate.get())) / pacing.as_secs_f64();
    GAIN_FLOOR_SECS..=(rate * (window_secs + ENDPOINT_SLACK_SECS))
}
