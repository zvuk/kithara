use std::num::{NonZeroU32, NonZeroUsize};

use kithara_audio::{AudioSource, Fetch, SeekOutcome, SourceEnd, TrackStep};
use kithara_command::{ChannelConfig, Sender, channel};
use kithara_effects::EffectDrain;
use kithara_platform::{
    sync::{Arc, Mutex},
    time::Duration,
};
use kithara_signal::{AudioChunk, AudioChunkInfo, AudioSpec};
use kithara_test_utils::bufpool::{TestPools, pools};
use kithara_warp::{Warp, WarpConfig};

use super::DecoderNode;
use crate::{LaneProtocol, WarpSource, mock::PacketRing, worker::PcmReceiver};

/// Creates a decoder node with the specified packet capacity and preload count.
///
/// # Panics
/// Panics if capacity or preload is zero, or fixture pool allocation fails.
pub async fn prepared_node<T>(
    source: T,
    capacity: usize,
    preload: usize,
) -> (DecoderNode<T, TestPools>, PcmReceiver, Sender<LaneProtocol>)
where
    T: AudioSource<Chunk = AudioChunk>,
{
    let spec = AudioSpec::new(2, NonZeroU32::new(44_100).expect("rate"));
    prepared_node_with_info(
        source,
        capacity,
        preload,
        AudioChunkInfo {
            spec,
            ..AudioChunkInfo::default()
        },
    )
    .await
}

pub(in crate::worker) async fn prepared_node_with_info<T>(
    source: T,
    capacity: usize,
    preload: usize,
    initial: AudioChunkInfo,
) -> (DecoderNode<T, TestPools>, PcmReceiver, Sender<LaneProtocol>)
where
    T: AudioSource<Chunk = AudioChunk>,
{
    prepared_node_with_cancel(source, capacity, preload, initial, None).await
}

pub(in crate::worker) async fn prepared_node_with_cancel<T>(
    source: T,
    capacity: usize,
    preload: usize,
    initial: AudioChunkInfo,
    cancel: Option<kithara_platform::CancelToken>,
) -> (DecoderNode<T, TestPools>, PcmReceiver, Sender<LaneProtocol>)
where
    T: AudioSource<Chunk = AudioChunk>,
{
    let pools = pools();
    let spec = initial.spec;
    let (lane, inbox) = channel(ChannelConfig::builder().build());
    let config = WarpConfig::builder()
        .source_block_frames(NonZeroUsize::new(65_536).expect("source block"))
        .build();
    let source = WarpSource::new(
        source,
        Warp::new((), &config).renderer(spec, pools.clone()),
        Vec::new(),
        EffectDrain::new(0, &pools).expect("drain"),
        spec,
        pools.clone(),
        crate::LaneSetup {
            inbox,
            preload_chunks: NonZeroUsize::new(preload).expect("preload"),
            declick: crate::consts::DEFAULT_DECLICK,
        },
    );
    let (receiver, producer) = PacketRing::new(spec, Duration::from_secs(1), capacity).into_ends();
    (
        DecoderNode::new(source, producer, None, initial, None, pools, cancel),
        receiver,
        lane,
    )
}

pub struct ScriptedSource {
    native_pcm_basis: Option<AudioChunkInfo>,
    pub(crate) landing: Option<Duration>,
    pub(crate) steps: std::collections::VecDeque<TrackStep<AudioChunk>>,
    pub(crate) commits: Arc<Mutex<Vec<SourceEnd>>>,
}

impl ScriptedSource {
    pub fn new<Steps>(steps: Steps) -> Self
    where
        Steps: IntoIterator<Item = TrackStep<AudioChunk>>,
    {
        Self {
            landing: None,
            native_pcm_basis: None,
            steps: steps.into_iter().collect(),
            commits: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

impl AudioSource for ScriptedSource {
    type Chunk = AudioChunk;
    fn commit_source_end(&mut self, end: SourceEnd, _meta: AudioChunkInfo) {
        self.commits.lock().push(end);
    }
    fn native_pcm_basis(&self) -> Option<AudioChunkInfo> {
        self.native_pcm_basis
    }
    fn host_sample_rate(&self) -> Option<NonZeroU32> {
        None
    }
    fn seek(&mut self, target: Duration) -> Result<SeekOutcome, kithara_audio::AudioReadError> {
        Ok(SeekOutcome::Landed {
            target,
            landed_at: self.landing.unwrap_or(target),
        })
    }
    fn seek_for_render(
        &mut self,
        target: Duration,
    ) -> Result<SeekOutcome, kithara_audio::AudioReadError> {
        self.seek(target)
    }
    fn visible_origin(&self) -> Option<Duration> {
        self.native_pcm_basis.and_then(|basis| basis.visible_origin)
    }
    fn set_host_sample_rate(&mut self, _rate: NonZeroU32) {}
    fn step_track(&mut self) -> TrackStep<AudioChunk> {
        let step = self.steps.pop_front().unwrap_or(TrackStep::Eof);
        if let TrackStep::Produced(Fetch::Data { data, .. }) = &step {
            self.native_pcm_basis = Some(data.meta);
        }
        step
    }
}
