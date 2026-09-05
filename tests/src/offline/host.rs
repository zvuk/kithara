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
        sync::{
            Arc, Mutex,
            atomic::{AtomicU64, Ordering},
        },
        time::{Duration, sleep},
    },
    play::{MixTapWriter, PlayError, TransportRevision, player::PlayerControlSource},
    queue::{Queue, QueueControl},
    signal::AudioSpec,
};
use ringbuf::{
    HeapCons, HeapRb,
    traits::{Consumer, Observer, Split},
};

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
    position: u64,
}

/// Test owner for the product offline Host and its monotonic render cursor.
pub struct OfflineHostHarness<S> {
    state: Mutex<HostState<S>>,
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
    P: PlayerControlSource<Schema = S>,
    S: HasPool<f32> + Send + Sync + 'static,
{
    pub fn new(config: HostConfig<S>, player: P) -> Result<Self, PlayError> {
        let host = OfflineHostHarness::new(config)?;
        let member = host.insert(player)?;
        Ok(Self { host, member })
    }

    pub fn render(&self, frames: usize) -> Vec<f32> {
        self.host.render(frames)
    }

    pub fn control(&self) -> P::Control {
        self.member.control().clone()
    }

    pub const fn host(&self) -> &OfflineHostHarness<S> {
        &self.host
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

/// Drive the periodic queue poll on the active test clock.
///
/// The flash gate keeps the spawned task on virtual time. Without it, buffered
/// sources can outrun the real-time ticker and trigger false hang timeouts.
#[kithara::flash(true)]
pub async fn drive_queue_ticks<S>(queue: QueueControl<S>, interval: Duration)
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    loop {
        sleep(interval).await;
        if queue.tick().is_err() {
            break;
        }
    }
}

impl<S> OfflineHostHarness<S>
where
    S: HasPool<f32> + Send + Sync + 'static,
{
    /// Build the same offline Host used by product rendering.
    pub fn new(config: HostConfig<S>) -> Result<Self, PlayError> {
        let spec = AudioSpec::new(CHANNELS, config.sample_rate());
        let max_block_frames = config
            .max_block_frames()
            .expect("offline Host config must have a render block size");
        let pacing = config.pacing();
        let host = Host::new(config)?;
        Ok(Self {
            state: Mutex::new(HostState { host, position: 0 }),
            spec,
            max_block_frames,
            pacing,
        })
    }

    /// Transfer one configured player facade into the product Host.
    pub fn insert<P>(&self, player: P) -> Result<HostOwned<P>, PlayError>
    where
        P: PlayerControlSource<Schema = S>,
    {
        self.state.lock().host.insert(player)
    }

    pub fn insert_control<P>(&self, player: P) -> Result<P::Control, PlayError>
    where
        P: PlayerControlSource<Schema = S>,
    {
        self.insert(player).map(|owned| owned.control().clone())
    }

    /// Render the next finite block through the product offline protocol.
    pub fn render(&self, frames: usize) -> Vec<f32> {
        let frames = u64::try_from(frames).expect("offline render frame count fits u64");
        let mut state = self.state.lock();
        let end = state
            .position
            .checked_add(frames)
            .expect("offline render timeline fits u64");
        let request = OfflineRenderRequest::builder()
            .spec(self.spec)
            .frames(state.position..end)
            .build();
        let cancel = CancelScope::new(None);
        let mut sink = VecSink::default();
        state
            .host
            .render(&request, &cancel.token(), &mut sink)
            .unwrap_or_else(|error| panic!("render product offline Host: {error}"));
        state.position = end;
        drop(state);
        sink.samples
    }

    /// Current finite-render cursor maintained by this harness.
    #[must_use]
    pub fn position(&self) -> u64 {
        self.state.lock().position
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

    pub fn enable_mix_tap(&self, capacity: usize) -> Result<MixTapProbe, PlayError> {
        let (pcm_tx, pcm_rx) = HeapRb::<f32>::new(capacity).split();
        let drops = Arc::new(AtomicU64::new(0));
        self.install_mix_tap(MixTapWriter::new(pcm_tx, Arc::clone(&drops)))?;
        Ok(MixTapProbe { drops, pcm: pcm_rx })
    }

    pub fn install_mix_tap(&self, writer: MixTapWriter) -> Result<(), PlayError> {
        let mut outputs = OutputGroup::new();
        outputs.push(writer);
        self.enable_outputs(outputs)
    }

    pub fn disable_mix_tap(&self) -> Result<(), PlayError> {
        self.state.lock().host.disable_outputs()
    }

    pub fn enable_outputs(&self, outputs: OutputGroup) -> Result<(), PlayError> {
        self.state.lock().host.enable_outputs(outputs)
    }

    pub fn restart_stream(&self, sample_rate: u32) -> Result<(), PlayError> {
        self.state.lock().host.restart_stream(sample_rate)
    }

    pub fn apply_mix<I>(&self, levels: I) -> Result<(), PlayError>
    where
        I: IntoIterator<Item = HostLevel>,
    {
        self.state.lock().host.apply_mix(levels)
    }

    pub fn transport_revision(&self) -> Result<TransportRevision, PlayError> {
        self.state.lock().host.transport_revision()
    }

    pub fn invalidate_audio_route(&self, reason: impl Into<String>) -> Result<(), PlayError> {
        self.state.lock().host.invalidate_audio_route(reason)
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
