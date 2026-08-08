use kithara_bufpool::PcmPool;
use kithara_decode::PcmSpec;
use kithara_platform::sync::Arc;
use kithara_stretch::{ElasticConfig, ElasticEngine, ElasticSpanConfig};

use super::{BoundError, BoundRenderer};
use crate::{musical::SourceSchedule, traits::AudioEffect};

/// Numeric policy the bound slot plans under.
struct Consts;

impl Consts {
    /// Source-frame tolerance when comparing adjacent continuous spans.
    const CONTINUITY_TOLERANCE: f64 = 1.0e-6;
    /// Source-frame correction the plan may apply across one block.
    const MAX_CORRECTION_PER_BLOCK: f64 = 0.25;
    /// Source-frame phase error accepted at a block boundary before the plan
    /// refuses to continue.
    const MAX_PHASE_ERROR: f64 = 0.5;
    /// Headroom over the block's output frames for the source span. The rate
    /// envelope any engine declares is well inside 2x, so a doubled block
    /// covers the widest span a block can ask for.
    const SOURCE_HEADROOM: u64 = 2;
}

/// Builds the exact-span slot for a bound deck on the engine this build has.
///
/// # Errors
///
/// Returns [`BoundError`] when the engine cannot be prepared for this shape.
pub(crate) fn bound_slot(
    schedule: Arc<SourceSchedule>,
    spec: PcmSpec,
    pool: PcmPool,
) -> Result<Box<dyn AudioEffect>, BoundError> {
    let output_frames = usize::try_from(BoundRenderer::<Engine>::BLOCK_FRAMES)
        .map_err(|_| BoundError::BlockOverflow)?;
    let source_frames =
        usize::try_from(BoundRenderer::<Engine>::BLOCK_FRAMES * Consts::SOURCE_HEADROOM)
            .map_err(|_| BoundError::BlockOverflow)?;
    let config = ElasticConfig::try_from((
        spec.sample_rate.get(),
        usize::from(spec.channels.max(1)),
        source_frames,
        output_frames,
    ))?;
    let engine = Engine::prepare(config)?;
    let span_config = ElasticSpanConfig::try_from((
        Consts::CONTINUITY_TOLERANCE,
        Consts::MAX_PHASE_ERROR,
        Consts::MAX_CORRECTION_PER_BLOCK,
    ))?;
    Ok(Box::new(BoundRenderer::new(
        schedule,
        engine,
        span_config,
        spec,
        pool,
    )))
}

/// Signalsmith is the exact-span engine wherever it is compiled in; bungee
/// serves the builds that carry only it.
#[cfg(feature = "stretch-signalsmith")]
type Engine = kithara_stretch::SignalsmithElastic;

#[cfg(all(not(feature = "stretch-signalsmith"), feature = "stretch-bungee"))]
type Engine = kithara_stretch::BungeeElastic;
