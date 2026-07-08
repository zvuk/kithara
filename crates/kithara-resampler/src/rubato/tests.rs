use std::num::{NonZeroU32, NonZeroUsize};

use kithara_bufpool::PcmPool;

use super::RubatoBackend;
use crate::{
    ResamplerBackend, ResamplerCapabilities, ResamplerConfig, ResamplerMode, ResamplerOptions,
    ResamplerSettings, create_resampler,
};

#[test]
fn rubato_backend_reports_fixed_ratio_standalone_support() {
    let capabilities = RubatoBackend::new().capabilities();

    assert!(capabilities.contains(ResamplerCapabilities::FIXED_RATIO));
    assert!(capabilities.contains(ResamplerCapabilities::STANDALONE));
}

#[test]
fn rubato_resamples_borrowed_planar_slices() {
    let channels = NonZeroUsize::new(2).unwrap_or_else(|| panic!("test channels"));
    let settings = ResamplerSettings::builder()
        .channels(channels)
        .mode(ResamplerMode::FixedRatio {
            source_sample_rate: NonZeroU32::new(48_000)
                .unwrap_or_else(|| panic!("test source rate")),
            target_sample_rate: NonZeroU32::new(44_100)
                .unwrap_or_else(|| panic!("test target rate")),
        })
        .options(ResamplerOptions::builder().chunk_size(256).build())
        .pcm_pool(PcmPool::new(4, 4_096))
        .build();
    let config = ResamplerConfig::builder()
        .backend(RubatoBackend::new())
        .settings(settings)
        .build();
    let mut resampler =
        create_resampler(&config).unwrap_or_else(|err| panic!("rubato build failed: {err}"));
    let input = [vec![0.1; 256], vec![0.2; 256]];
    let mut output: [Vec<f32>; 2] =
        std::array::from_fn(|_| vec![0.0; resampler.output_frames_next()]);
    let input_refs = [&input[0][..], &input[1][..]];
    let mut output_refs = output.iter_mut().map(Vec::as_mut_slice).collect::<Vec<_>>();

    let process = resampler
        .process_into_buffer(&input_refs, &mut output_refs)
        .unwrap_or_else(|err| panic!("rubato process failed: {err}"));

    assert_eq!(process.input_frames, resampler.input_frames_next());
    assert!(process.output_frames > 0);
}
