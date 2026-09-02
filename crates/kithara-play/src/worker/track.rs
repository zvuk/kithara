use std::num::NonZeroUsize;

use bon::Builder;
use kithara_audio::{AudioConfig, ResamplerBackend};
use kithara_platform::sync::Arc;
use kithara_stream::StreamType;
use kithara_warp::WarpConfig;

use super::EngineLoad;
use crate::effects::AudioEffect;

struct Consts;

impl Consts {
    const NATIVE_AUDIO_BUFFER_FRAMES: usize = 5_120;
    const NATIVE_AUDIO_BUFFER_CHUNKS: usize = 10;
    const WASM_AUDIO_BUFFER_CHUNKS: usize = 32;
}

fn resolve_audio_buffer_chunks(
    configured_chunks: Option<usize>,
    render_quantum_frames: NonZeroUsize,
) -> usize {
    configured_chunks.unwrap_or_else(|| default_audio_buffer_chunks(render_quantum_frames))
}

fn default_audio_buffer_chunks(render_quantum_frames: NonZeroUsize) -> usize {
    if cfg!(target_arch = "wasm32") {
        Consts::WASM_AUDIO_BUFFER_CHUNKS
    } else if cfg!(any(
        feature = "stretch-signalsmith",
        feature = "stretch-bungee"
    )) {
        Consts::NATIVE_AUDIO_BUFFER_FRAMES.div_ceil(render_quantum_frames.get())
    } else {
        Consts::NATIVE_AUDIO_BUFFER_CHUNKS
    }
}

/// Play-owned configuration for one resident Warp/audio producer lane.
#[derive(Builder, fieldwork::Fieldwork)]
#[builder(start_fn = for_audio)]
#[fieldwork(opt_in, get)]
#[non_exhaustive]
pub struct TrackConfig<T, B>
where
    T: StreamType,
    B: ResamplerBackend,
{
    /// Source-only decoder configuration.
    #[builder(start_fn)]
    #[field(get)]
    pub(crate) audio: AudioConfig<T, B>,
    /// Optional live cost meter for this play-owned producer lane.
    #[field(get)]
    pub(crate) engine_load: Option<Arc<EngineLoad>>,
    /// Playback effects after the resident Warp stage.
    #[builder(default)]
    #[field(get)]
    pub(crate) effects: Vec<Box<dyn AudioEffect>>,
    /// Resident Warp resources and live temporal controls.
    #[builder(default = WarpConfig::builder().build())]
    #[field(get)]
    pub(crate) warp: WarpConfig,
}

impl<T, B> TrackConfig<T, B>
where
    T: StreamType,
    B: ResamplerBackend,
{
    pub(crate) fn resolved_audio_buffer_chunks(&self) -> usize {
        resolve_audio_buffer_chunks(
            self.audio.audio_buffer_chunks(),
            self.warp.render_quantum_frames(),
        )
    }
}

impl<T, B> From<AudioConfig<T, B>> for TrackConfig<T, B>
where
    T: StreamType,
    B: ResamplerBackend,
{
    fn from(audio: AudioConfig<T, B>) -> Self {
        Self::for_audio(audio).build()
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;

    use kithara_test_utils::kithara;

    use super::resolve_audio_buffer_chunks;

    #[kithara::test(native)]
    #[case::legacy_quantum(None, 512, 10)]
    #[case::response_quantum(None, 64, 80)]
    #[case::explicit_override(Some(2), 64, 2)]
    fn audio_buffer_horizon_is_resolved_in_frames(
        #[case] configured_chunks: Option<usize>,
        #[case] quantum_frames: usize,
        #[case] expected_chunks: usize,
    ) {
        let quantum_frames = NonZeroUsize::new(quantum_frames).expect("test quantum is non-zero");

        assert_eq!(
            resolve_audio_buffer_chunks(configured_chunks, quantum_frames),
            expected_chunks
        );
    }
}
