use std::{
    collections::VecDeque,
    num::{NonZeroU32, NonZeroUsize},
};

use kithara_audio::{Fetch, TrackStep, WaitingReason};
use kithara_bufpool::PoolRegion;
use kithara_command::{Batch, ChannelConfig, Inbox, Sender, channel};
use kithara_platform::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};
use kithara_signal::AudioChunkInfo;
use kithara_test_utils::bufpool::TestPools;
#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
use kithara_warp::WarpCapabilities;
use kithara_warp::{SpeedCurve, StretchKind};
#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
use num_traits::AsPrimitive;

use super::{consts, *};
use crate::{LaneCommand, LaneProtocol};

#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
pub(in crate::source) fn keylock_backends() -> impl Iterator<Item = StretchKind> {
    StretchKind::all()
        .iter()
        .copied()
        .filter(|backend| backend.capabilities().contains(WarpCapabilities::KEYLOCK))
}

pub(in crate::source) fn flush_deferred<S>(source: &mut S)
where
    S: AudioSource,
{
    let _ = source.prepare_deferred();
    source.finish_deferred();
}

/// The inbox of a lane no player sends to.
pub(in crate::source) fn idle_inbox() -> Inbox<LaneProtocol> {
    channel::<LaneProtocol>(ChannelConfig::builder().build()).1
}

pub(in crate::source) fn source_stage<T>(
    pools: &PoolRegion<TestPools>,
    source: T,
    effects: Vec<Box<dyn AudioEffect>>,
    spec: AudioSpec,
) -> WarpSource<T, TestPools>
where
    T: AudioSource<Chunk = AudioChunk>,
{
    source_stage_with_quantum(pools, source, effects, spec, 128)
}

pub(in crate::source) fn source_stage_with_quantum<T>(
    pools: &PoolRegion<TestPools>,
    source: T,
    effects: Vec<Box<dyn AudioEffect>>,
    spec: AudioSpec,
    quantum_frames: usize,
) -> WarpSource<T, TestPools>
where
    T: AudioSource<Chunk = AudioChunk>,
{
    let config = kithara_warp::WarpConfig::builder()
        .render_quantum_frames(NonZeroUsize::new(quantum_frames).expect("test quantum is non-zero"))
        .build();
    let warp = kithara_warp::Warp::new((), &config);
    let renderer = warp.renderer(spec, pools.clone());
    let drain = EffectDrain::new(effects.len(), pools)
        .unwrap_or_else(|error| panic!("test effect drain: {error}"));
    WarpSource::new(
        source,
        renderer,
        effects,
        drain,
        spec,
        pools.clone(),
        LaneSetup {
            inbox: idle_inbox(),
            preload_chunks: NonZeroUsize::new(1).expect("preload"),
            declick: consts::DEFAULT_DECLICK,
        },
    )
}

pub(in crate::source) struct RawSource {
    pub(in crate::source) head: Arc<AtomicU64>,
    pub(in crate::source) chunks: VecDeque<AudioChunk>,
}

impl AudioSource for RawSource {
    type Chunk = AudioChunk;

    fn seek(&mut self, target: Duration) -> Result<SeekOutcome, AudioReadError> {
        Ok(SeekOutcome::Landed {
            target,
            landed_at: target,
        })
    }

    fn set_host_sample_rate(&mut self, _rate: NonZeroU32) {}

    fn host_sample_rate(&self) -> Option<NonZeroU32> {
        None
    }

    fn step_track(&mut self) -> TrackStep<AudioChunk> {
        let Some(chunk) = self.chunks.pop_front() else {
            return TrackStep::Eof;
        };
        self.head.store(
            chunk
                .meta
                .frame_offset
                .saturating_add(u64::from(chunk.meta.frames)),
            Ordering::Release,
        );
        TrackStep::Produced(Fetch::data(chunk))
    }
}

#[cfg(any(
    feature = "stretch-identity",
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
pub(in crate::source) struct FailedSource {
    pub(in crate::source) failure: TrackFailureKind,
    pub(in crate::source) chunks: VecDeque<AudioChunk>,
}

#[cfg(any(
    feature = "stretch-identity",
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
impl AudioSource for FailedSource {
    type Chunk = AudioChunk;

    fn set_host_sample_rate(&mut self, _rate: NonZeroU32) {}

    fn host_sample_rate(&self) -> Option<NonZeroU32> {
        None
    }

    fn seek(&mut self, position: Duration) -> Result<SeekOutcome, AudioReadError> {
        Ok(SeekOutcome::Landed {
            target: position,
            landed_at: position,
        })
    }

    fn step_track(&mut self) -> TrackStep<AudioChunk> {
        self.chunks
            .pop_front()
            .map_or(TrackStep::Failed(self.failure), |chunk| {
                TrackStep::Produced(Fetch::data(chunk))
            })
    }
}

#[derive(Default)]
pub(in crate::source) struct BufferThenHalveFrames {
    pub(in crate::source) buffered: Option<AudioChunk>,
}

impl AudioEffect for BufferThenHalveFrames {
    fn held_source_frames(&self) -> u64 {
        self.buffered
            .as_ref()
            .map_or(0, |chunk| u64::from(chunk.meta.frames))
    }

    fn reset(&mut self) {
        self.buffered = None;
    }

    delegate::delegate! {
        to self.buffered {
            #[expr($.and_then(halve_frames))]
            #[call(take)]
            fn flush(&mut self) -> Option<AudioChunk>;
            #[expr($.and_then(halve_frames))]
            #[call(replace)]
            fn process(&mut self, chunk: AudioChunk) -> Option<AudioChunk>;
        }
    }
}

pub(in crate::source) fn halve_frames(mut chunk: AudioChunk) -> Option<AudioChunk> {
    let frames = chunk.meta.frames / 2;
    let samples = usize::try_from(frames)
        .ok()?
        .checked_mul(usize::from(chunk.meta.spec.channels))?;
    chunk.samples.truncate(samples);
    chunk.meta.frames = frames;
    chunk.meta.end_timestamp = chunk
        .meta
        .spec
        .duration_for(chunk.meta.frame_offset.saturating_add(u64::from(frames)))
        .expect("fixture timestamp fits");
    Some(chunk)
}

pub(in crate::source) struct DeferredSource {
    pub(in crate::source) log: Arc<Mutex<Vec<&'static str>>>,
    pub(in crate::source) spec: AudioSpec,
}

impl AudioSource for DeferredSource {
    type Chunk = AudioChunk;

    fn seek(&mut self, target: Duration) -> Result<SeekOutcome, AudioReadError> {
        Ok(SeekOutcome::Landed {
            target,
            landed_at: target,
        })
    }

    fn set_host_sample_rate(&mut self, _rate: NonZeroU32) {}

    fn host_sample_rate(&self) -> Option<NonZeroU32> {
        None
    }

    fn finish_deferred(&mut self) {
        self.log.lock().push("source.finish");
    }

    fn prepare_deferred(&mut self) -> Option<AudioSpec> {
        self.log.lock().push("source.prepare");
        Some(self.spec)
    }

    fn step_track(&mut self) -> TrackStep<AudioChunk> {
        TrackStep::Blocked(WaitingReason::Waiting)
    }
}

pub(in crate::source) struct DeferredEffect {
    pub(in crate::source) log: Arc<Mutex<Vec<&'static str>>>,
    pub(in crate::source) serviced: Arc<Mutex<Option<AudioSpec>>>,
}

impl AudioEffect for DeferredEffect {
    fn flush(&mut self) -> Option<AudioChunk> {
        None
    }

    fn held_source_frames(&self) -> u64 {
        0
    }

    fn process(&mut self, chunk: AudioChunk) -> Option<AudioChunk> {
        Some(chunk)
    }

    fn reset(&mut self) {}

    fn service_deferred(&mut self, spec: AudioSpec) {
        self.log.lock().push("effect.service");
        *self.serviced.lock() = Some(spec);
    }
}

pub(in crate::source) struct RevisionSource {
    pub(in crate::source) discontinuity: Arc<Mutex<SourceDiscontinuity>>,
    pub(in crate::source) chunks: VecDeque<AudioChunk>,
}

impl AudioSource for RevisionSource {
    type Chunk = AudioChunk;

    fn seek(&mut self, target: Duration) -> Result<SeekOutcome, AudioReadError> {
        Ok(SeekOutcome::Landed {
            target,
            landed_at: target,
        })
    }

    fn set_host_sample_rate(&mut self, _rate: NonZeroU32) {}

    fn host_sample_rate(&self) -> Option<NonZeroU32> {
        None
    }

    fn discontinuity(&self) -> Option<SourceDiscontinuity> {
        Some(*self.discontinuity.lock())
    }

    fn step_track(&mut self) -> TrackStep<AudioChunk> {
        self.chunks
            .pop_front()
            .map_or(TrackStep::Blocked(WaitingReason::Waiting), |chunk| {
                TrackStep::Produced(Fetch::data(chunk))
            })
    }
}

pub(in crate::source) struct ResetCounter {
    pub(in crate::source) resets: Arc<AtomicU64>,
}

impl AudioEffect for ResetCounter {
    fn flush(&mut self) -> Option<AudioChunk> {
        None
    }

    fn held_source_frames(&self) -> u64 {
        0
    }

    fn process(&mut self, chunk: AudioChunk) -> Option<AudioChunk> {
        Some(chunk)
    }

    fn reset(&mut self) {
        self.resets.fetch_add(1, Ordering::AcqRel);
    }
}

pub(in crate::source) struct CountingEofSource {
    pub(in crate::source) discontinuity: Arc<Mutex<Option<SourceDiscontinuity>>>,
    pub(in crate::source) steps: Arc<AtomicU64>,
}

impl AudioSource for CountingEofSource {
    type Chunk = AudioChunk;

    fn seek(&mut self, target: Duration) -> Result<SeekOutcome, AudioReadError> {
        let revision = self
            .discontinuity
            .lock()
            .map_or(0, |stamp| stamp.revision())
            + 1;
        *self.discontinuity.lock() = Some(SourceDiscontinuity::new(
            revision,
            AudioSpec::new(2, NonZeroU32::new(44_100).expect("rate")),
        ));
        Ok(SeekOutcome::Landed {
            target,
            landed_at: target,
        })
    }

    fn set_host_sample_rate(&mut self, _rate: NonZeroU32) {}

    fn host_sample_rate(&self) -> Option<NonZeroU32> {
        None
    }

    fn discontinuity(&self) -> Option<SourceDiscontinuity> {
        *self.discontinuity.lock()
    }

    fn step_track(&mut self) -> TrackStep<AudioChunk> {
        self.steps.fetch_add(1, Ordering::AcqRel);
        TrackStep::Eof
    }
}

pub(in crate::source) struct CountingEmptyTail {
    pub(in crate::source) flushes: Arc<AtomicU64>,
    pub(in crate::source) resets: Arc<AtomicU64>,
}

impl AudioEffect for CountingEmptyTail {
    fn flush(&mut self) -> Option<AudioChunk> {
        self.flushes.fetch_add(1, Ordering::AcqRel);
        None
    }

    fn held_source_frames(&self) -> u64 {
        0
    }

    fn process(&mut self, chunk: AudioChunk) -> Option<AudioChunk> {
        Some(chunk)
    }

    fn reset(&mut self) {
        self.resets.fetch_add(1, Ordering::AcqRel);
    }
}

pub(in crate::source) struct SeekApplyingSource {
    pub(in crate::source) spec: AudioSpec,
    pub(in crate::source) revision: u64,
    pub(in crate::source) pending: bool,
}

impl AudioSource for SeekApplyingSource {
    type Chunk = AudioChunk;

    fn seek(&mut self, target: Duration) -> Result<SeekOutcome, AudioReadError> {
        self.revision = self.revision.wrapping_add(1);
        self.pending = true;
        Ok(SeekOutcome::Landed {
            target,
            landed_at: target,
        })
    }

    fn set_host_sample_rate(&mut self, _rate: NonZeroU32) {}

    fn host_sample_rate(&self) -> Option<NonZeroU32> {
        None
    }

    fn discontinuity(&self) -> Option<SourceDiscontinuity> {
        Some(SourceDiscontinuity::new(self.revision, self.spec))
    }

    fn step_track(&mut self) -> TrackStep<AudioChunk> {
        if std::mem::take(&mut self.pending) {
            TrackStep::StateChanged
        } else {
            TrackStep::Eof
        }
    }
}

pub(in crate::source) struct ResettingTail {
    pub(in crate::source) resets: Arc<AtomicU64>,
    pub(in crate::source) tail: Option<AudioChunk>,
}

impl AudioEffect for ResettingTail {
    fn flush(&mut self) -> Option<AudioChunk> {
        self.tail.take()
    }

    fn held_source_frames(&self) -> u64 {
        self.tail
            .as_ref()
            .map_or(0, |chunk| u64::from(chunk.meta.frames))
    }

    fn process(&mut self, chunk: AudioChunk) -> Option<AudioChunk> {
        Some(chunk)
    }

    fn reset(&mut self) {
        self.tail = None;
        self.resets.fetch_add(1, Ordering::AcqRel);
    }
}

pub(in crate::source) fn chunk(
    pools: &PoolRegion<TestPools>,
    spec: AudioSpec,
    frame_offset: u64,
    input: &[f32],
) -> AudioChunk {
    const FRAMES: usize = 128;
    chunk_with_frames(
        pools,
        spec,
        frame_offset,
        u32::try_from(FRAMES).expect("fixture frames fit u32"),
        input,
    )
}

pub(in crate::source) fn chunk_with_frames(
    pools: &PoolRegion<TestPools>,
    spec: AudioSpec,
    frame_offset: u64,
    frames: u32,
    input: &[f32],
) -> AudioChunk {
    let samples = usize::try_from(frames)
        .expect("fixture frames fit usize")
        .checked_mul(usize::from(spec.channels))
        .expect("fixture sample count fits usize");
    let mut buffer = pools
        .get_with_len::<f32>(samples)
        .unwrap_or_else(|error| panic!("test sample buffer: {error}"));
    buffer.copy_from_slice(&input[..samples]);
    AudioChunk::new(
        AudioChunkInfo {
            spec,
            frames,
            frame_offset,
            timestamp: spec
                .duration_for(frame_offset)
                .expect("fixture timestamp fits"),
            end_timestamp: spec
                .duration_for(frame_offset.saturating_add(u64::from(frames)))
                .expect("fixture end timestamp fits"),
            ..Default::default()
        },
        buffer,
    )
}

/// One emitted chunk on the lane axis and the source span it renders.
pub(in crate::source) struct Emitted {
    pub(in crate::source) lane_start: u64,
    pub(in crate::source) source_start: u64,
    pub(in crate::source) revision: u64,
    pub(in crate::source) samples: Vec<f32>,
}

pub(in crate::source) fn speed_batch(speed: f32) -> Batch<LaneProtocol> {
    command_batch(LaneCommand::SetSpeed(SpeedCurve::Constant(speed)))
}

pub(in crate::source) fn command_batch(command: LaneCommand) -> Batch<LaneProtocol> {
    Batch {
        basis: Vec::new(),
        commands: vec![command],
    }
}

/// A lane over constant source audio at unity speed, with the player end
/// of its channel.
pub(in crate::source) fn speed_lane(
    pools: &PoolRegion<TestPools>,
    quarter: &[f32],
) -> (WarpSource<RawSource, TestPools>, Sender<LaneProtocol>) {
    lane_over(pools, 3, |_| quarter, 1.0, (StretchKind::default(), false))
}

/// A lane over `signal` at unity speed, rendered by `backend` with keylock
/// when `keylock`.
#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
pub(in crate::source) fn stretch_lane(
    pools: &PoolRegion<TestPools>,
    (backend, keylock): (StretchKind, bool),
    signal: &[f32],
) -> (WarpSource<RawSource, TestPools>, Sender<LaneProtocol>) {
    let chunks = signal.len() / (2 * consts::LANE_CHUNK_FRAMES as usize);
    lane_over(
        pools,
        u32::try_from(chunks).expect("test chunk count fits u32"),
        |index| &signal[index as usize * 2 * consts::LANE_CHUNK_FRAMES as usize..],
        1.0,
        (backend, keylock),
    )
}

/// A lane over `chunks` source chunks of [`consts::LANE_CHUNK_FRAMES`] frames, the
/// `index`th copied from the front of `samples(index)`, starting at `speed` on
/// `backend`, with keylock when `keylock`.
pub(in crate::source) fn lane_over<'a>(
    pools: &PoolRegion<TestPools>,
    chunks: u32,
    samples: impl Fn(u32) -> &'a [f32],
    speed: f32,
    (backend, keylock): (StretchKind, bool),
) -> (WarpSource<RawSource, TestPools>, Sender<LaneProtocol>) {
    let spec = AudioSpec::new(2, NonZeroU32::new(44_100).expect("test sample rate"));
    let chunks = (0..chunks)
        .map(|index| {
            chunk_with_frames(
                pools,
                spec,
                u64::from(index * consts::LANE_CHUNK_FRAMES),
                consts::LANE_CHUNK_FRAMES,
                samples(index),
            )
        })
        .collect();
    let raw = RawSource {
        chunks,
        head: Arc::new(AtomicU64::new(0)),
    };
    let (lane, inbox) = channel::<LaneProtocol>(ChannelConfig::builder().build());
    let config = kithara_warp::WarpConfig::builder()
        .speed(speed)
        .keylock(keylock)
        .backend(backend)
        .render_quantum_frames(NonZeroUsize::new(256).expect("test quantum is non-zero"))
        .build();
    let renderer = kithara_warp::Warp::new((), &config).renderer(spec, pools.clone());
    let drain =
        EffectDrain::new(0, pools).unwrap_or_else(|error| panic!("test effect drain: {error}"));
    let source = WarpSource::new(
        raw,
        renderer,
        Vec::new(),
        drain,
        spec,
        pools.clone(),
        LaneSetup {
            inbox,
            preload_chunks: NonZeroUsize::new(1).expect("preload"),
            declick: consts::DEFAULT_DECLICK,
        },
    );
    (source, lane)
}

/// Steps the lane from output frame `from` until it emitted `until`.
pub(in crate::source) fn emit(
    source: &mut WarpSource<RawSource, TestPools>,
    from: u64,
    until: u64,
) -> Vec<Emitted> {
    let mut emitted = Vec::new();
    let mut cursor = from;
    for _ in 0..4096 {
        if cursor >= until {
            return emitted;
        }
        match source.step_track() {
            TrackStep::Produced(Fetch::Data { data, .. }) => {
                emitted.push(Emitted {
                    lane_start: cursor,
                    source_start: data.meta.frame_offset,
                    revision: data.meta.render_revision,
                    samples: data.samples.to_vec(),
                });
                cursor += u64::from(data.meta.frames);
            }
            TrackStep::StateChanged | TrackStep::Blocked(_) => {}
            TrackStep::Eof => panic!("the lane ended at frame {cursor} before {until}"),
            _ => panic!("the lane failed at frame {cursor} before {until}"),
        }
        flush_deferred(source);
    }
    panic!("the lane stalled at frame {cursor} before {until}");
}

/// A stereo linear chirp, 220 Hz to 1760 Hz: every source frame sounds a
/// frequency of its own, so audio rendered from another frame decorrelates.
#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
pub(in crate::source) fn chirp(frames: usize) -> Vec<f32> {
    let rate = 44_100.0_f64;
    let length: f64 = frames.as_();
    let span = length / rate;
    (0..frames)
        .flat_map(|frame| {
            let frame: f64 = frame.as_();
            let time = frame / rate;
            let phase = std::f64::consts::TAU
                * time.mul_add(220.0, (1_760.0 - 220.0) / (2.0 * span) * time * time);
            let sample: f32 = (0.5 * phase.sin()).as_();
            [sample, sample]
        })
        .collect()
}

/// A lane frame as an index into [`lane_pcm`].
#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
pub(in crate::source) fn pcm_index(frame: u64) -> usize {
    usize::try_from(frame).expect("test lane frame fits usize")
}

/// The left channel the lane emitted, indexed by lane frame.
#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
pub(in crate::source) fn lane_pcm(emitted: &[Emitted]) -> Vec<f32> {
    emitted
        .iter()
        .flat_map(|chunk| chunk.samples.iter().step_by(2).copied())
        .collect()
}

/// The offset of `rendered` within `reference` around `center` that
/// correlates best, within `reach` frames, and that correlation.
#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
pub(in crate::source) fn alignment(
    rendered: &[f32],
    reference: &[f32],
    center: usize,
    reach: usize,
) -> (i64, f64) {
    let energy = |samples: &[f32]| {
        samples
            .iter()
            .map(|&sample| f64::from(sample) * f64::from(sample))
            .sum::<f64>()
    };
    let rendered_energy = energy(rendered);
    (0..=2 * reach)
        .map(|shift| {
            let window = &reference[center - reach + shift..][..rendered.len()];
            let product = rendered
                .iter()
                .zip(window)
                .map(|(&left, &right)| f64::from(left) * f64::from(right))
                .sum::<f64>();
            let correlation = product / (rendered_energy * energy(window)).sqrt();
            (shift as i64 - reach as i64, correlation)
        })
        .fold(
            (0, f64::MIN),
            |best, next| {
                if next.1 > best.1 { next } else { best }
            },
        )
}

/// A stereo 440 Hz sine at half scale: a keylock engine keeps its pitch at
/// any speed, so its output moves between two samples by at most the
/// sine's own step.
#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
pub(in crate::source) fn sine(frames: usize) -> Vec<f32> {
    (0..frames)
        .flat_map(|frame| {
            let frame: f64 = frame.as_();
            let phase = std::f64::consts::TAU * 440.0 * frame / 44_100.0;
            let sample: f32 = (0.5 * phase.sin()).as_();
            [sample, sample]
        })
        .collect()
}
