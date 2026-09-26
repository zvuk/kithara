use bungee_sys::Request;
use kithara_bufpool::HasPool;
use kithara_signal::PlanarBuffer;

use super::{
    buffer::{InputBuffer, planar_buffer},
    ffi::{NativeOutput, NativeStretcher},
};
use crate::{ElasticConfig, ElasticError, ElasticRateEnvelope};

#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in)]
pub(super) struct StreamCore {
    pub(super) rate_envelope: ElasticRateEnvelope,
    pub(super) input: InputBuffer,
    pub(super) native: NativeStretcher,
    pub(super) anchor: Option<f64>,
    pub(super) output_chunk: Option<NativeOutput>,
    pub(super) output: PlanarBuffer,
    pub(super) request: Request,
    pub(super) cue_grain_pending: bool,
    pub(super) request_pending: bool,
    pub(super) unprimed_started: bool,
    pub(super) grain_output_position: f64,
    pub(super) samples_needed: f64,
    pub(super) audible_output_end: u64,
    pub(super) audible_source_end: u64,
    pub(super) output_consumed: usize,
    pub(super) source_latency_frames: usize,
    #[field(get, copy, vis = "pub(super)")]
    max_input_frames: usize,
}

impl StreamCore {
    pub(super) const PIPELINE_GRAINS: usize = 4;
    pub(super) const TERMINAL_GRAIN_LIMIT: usize = 64;

    pub(super) fn new<S>(
        config: &ElasticConfig<S>,
        max_source_frames: usize,
    ) -> Result<Self, ElasticError>
    where
        S: HasPool<f32>,
    {
        let native = NativeStretcher::new(
            config.sample_rate(),
            config.channels(),
            *config.backends().bungee(),
        )?;
        let max_input_frames = native.max_input_frames()?;
        let source_latency_frames = max_input_frames / 2;
        Ok(Self {
            anchor: None,
            audible_source_end: 0,
            audible_output_end: 0,
            grain_output_position: 0.0,
            cue_grain_pending: false,
            input: InputBuffer::new(config, max_input_frames, max_source_frames)?,
            max_input_frames,
            native,
            output: planar_buffer(config, max_input_frames)?,
            output_chunk: None,
            output_consumed: 0,
            request: Request {
                position: f64::NAN,
                speed: 1.0,
                pitch: 1.0,
                reset: 0,
            },
            request_pending: false,
            rate_envelope: config.rate_envelope(),
            samples_needed: 0.0,
            source_latency_frames,
            unprimed_started: false,
        })
    }

    pub(super) fn prepare_input_capacity(&mut self, capacity: usize) -> Result<(), ElasticError> {
        self.input.prepare_source_capacity(capacity)
    }

    pub(super) fn set_source_latency_frames(&mut self, frames: usize) -> Result<(), ElasticError> {
        if frames == 0 || frames > self.max_input_frames / 2 {
            return Err(ElasticError::EnginePreparation(
                "Bungee reported an invalid source latency",
            ));
        }
        self.source_latency_frames = frames;
        Ok(())
    }

    pub(super) fn source_latency_frames(&self) -> Result<usize, ElasticError> {
        let (history, lookahead) = self.input.requested_window(self.request.position)?;
        if history > 0 && history == lookahead {
            return Ok(history);
        }
        Err(ElasticError::EnginePreparation(
            "Bungee reported an unsupported asymmetric input window",
        ))
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_fixtures::stretch_fixtures::{StretchPcm, stretch_pcm};
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{ElasticRequest, backends::bungee::ffi::NativeFault, consts, test_pools::pools};

    fn signal(pcm: &StretchPcm, frames: usize, offset: usize) -> Vec<f32> {
        pcm.bungee[offset * consts::CHANNELS..(offset + frames) * consts::CHANNELS].to_vec()
    }

    fn anchored_core(stretch_pcm: &StretchPcm) -> StreamCore {
        let config = ElasticConfig::builder()
            .pools(pools())
            .sample_rate(consts::SAMPLE_RATE)
            .channels(consts::CHANNELS)
            .max_source_frames(consts::CONTEXT_FRAMES)
            .max_output_frames(consts::CONTEXT_FRAMES)
            .build()
            .expect("the fixture shape is valid");
        let mut core = StreamCore::new(&config, consts::CONTEXT_FRAMES)
            .expect("the anchored fixture core prepares");
        core.prepare_input_capacity(consts::CONTEXT_FRAMES * 4)
            .expect("the fixture reserves its complete prime context");
        let history = signal(stretch_pcm, consts::CONTEXT_FRAMES, 0);
        let lookahead = signal(stretch_pcm, consts::CONTEXT_FRAMES, consts::CONTEXT_FRAMES);
        let warm_source = signal(
            stretch_pcm,
            consts::CONTEXT_FRAMES,
            consts::CONTEXT_FRAMES * 2,
        );
        let mut discarded = vec![0.0; consts::CONTEXT_FRAMES * consts::CHANNELS];
        core.prime(
            &history,
            &lookahead,
            ElasticRequest::new(consts::CONTEXT_FRAMES, consts::CONTEXT_FRAMES)
                .expect("the warmup request is valid"),
            &warm_source,
            1.0,
            &mut discarded,
        )
        .expect("the fixture primes at the cue");
        core
    }

    #[kithara::test]
    fn render_call_does_not_prefetch_a_grain_with_stale_controls(stretch_pcm: &'static StretchPcm) {
        const FRAMES: usize = 8192;

        let config = ElasticConfig::builder()
            .pools(pools())
            .sample_rate(48_000)
            .channels(2)
            .max_source_frames(FRAMES)
            .max_output_frames(FRAMES)
            .build()
            .expect("the fixture shape is valid");
        let mut core = StreamCore::new(&config, FRAMES).expect("the fixture core prepares");
        let request = ElasticRequest::new(FRAMES, FRAMES).expect("unity request");
        let source = stretch_pcm.silence[..FRAMES * config.channels()].to_vec();
        let mut output = vec![0.0; source.len()];

        core.render(Some(&source), request, 1.0, Some(&mut output))
            .expect("the first quantum renders");

        assert!(
            !core.request_pending,
            "no grain may remain specified with controls from a previous render call"
        );
    }

    #[kithara::test]
    fn adjacent_unprimed_rate_extremes_do_not_reset_native_continuity(
        stretch_pcm: &'static StretchPcm,
    ) {
        const FAST_OUTPUT_FRAMES: usize = 2048;
        const FAST_SOURCE_FRAMES: usize = 8192;
        const SLOW_OUTPUT_FRAMES: usize = 8000;
        const SLOW_SOURCE_FRAMES: usize = 400;

        let config = ElasticConfig::builder()
            .pools(pools())
            .sample_rate(consts::SAMPLE_RATE)
            .channels(consts::CHANNELS)
            .max_source_frames(consts::CONTEXT_FRAMES)
            .max_output_frames(consts::CONTEXT_FRAMES)
            .build()
            .expect("the fixture shape is valid");
        let mut core =
            StreamCore::new(&config, consts::CONTEXT_FRAMES).expect("the fixture core prepares");
        let probe = ElasticRequest::new(consts::CONTEXT_FRAMES, consts::CONTEXT_FRAMES)
            .expect("the latency probe request is valid");
        for _ in 0..StreamCore::PIPELINE_GRAINS {
            core.probe_silence(probe)
                .expect("the latency probe renders");
        }
        let source_latency = core
            .source_latency_frames()
            .expect("the native source window is measurable");
        core.set_source_latency_frames(source_latency)
            .expect("the measured source latency is valid");
        core.discard().expect("the latency probe clears");
        let slow = ElasticRequest::new(SLOW_SOURCE_FRAMES, SLOW_OUTPUT_FRAMES)
            .expect("the slow request is valid");
        let slow_source = signal(stretch_pcm, SLOW_SOURCE_FRAMES, 0);
        let mut slow_output = vec![0.0; SLOW_OUTPUT_FRAMES * consts::CHANNELS];
        core.render(Some(&slow_source), slow, 1.0, Some(&mut slow_output))
            .expect("the slow request renders");
        let slow_position = core.request.position;

        let fast = ElasticRequest::new(FAST_SOURCE_FRAMES, FAST_OUTPUT_FRAMES)
            .expect("the fast request is valid");
        let fast_source = signal(stretch_pcm, FAST_SOURCE_FRAMES, SLOW_SOURCE_FRAMES);
        let mut fast_output = vec![0.0; FAST_OUTPUT_FRAMES * consts::CHANNELS];
        core.render(Some(&fast_source), fast, 1.0, Some(&mut fast_output))
            .expect("the adjacent fast request renders");

        assert_eq!(core.request.reset, 0);
        assert!(core.request.position > slow_position);
        assert!(fast_output.iter().all(|sample| sample.is_finite()));
    }

    #[kithara::test]
    fn adjacent_control_changes_keep_the_original_anchor_and_native_history(
        stretch_pcm: &'static StretchPcm,
    ) {
        const QUANTUM: usize = 64;
        const TRANSITIONS: usize = 32;

        let mut core = anchored_core(stretch_pcm);
        let mut source_position = consts::CONTEXT_FRAMES * 3;
        let mut previous: Option<[f32; consts::CHANNELS]> = None;
        for transition in 0..TRANSITIONS {
            let fast = transition.is_multiple_of(2);
            let source_frames = if fast { QUANTUM * 2 } else { QUANTUM };
            let pitch = if fast { 1.5 } else { 1.0 };
            let source = signal(stretch_pcm, source_frames, source_position);
            let mut output = vec![f32::NAN; QUANTUM * consts::CHANNELS];
            core.render(
                Some(&source),
                ElasticRequest::new(source_frames, QUANTUM)
                    .expect("the alternating request is non-empty"),
                pitch,
                Some(&mut output),
            )
            .expect("the adjacent control quantum renders");

            assert_eq!(core.anchor, Some(0.0));
            assert_eq!(core.request.reset, 0);
            assert!(!core.native.is_flushed());
            assert!(output.iter().all(|sample| sample.is_finite()));
            if let Some(previous) = previous {
                for channel in 0..consts::CHANNELS {
                    let after = output[channel];
                    assert!(
                        (after - previous[channel]).abs() <= 0.1,
                        "transition {transition} must preserve phase continuity on channel {channel}"
                    );
                }
            }
            previous = Some([
                output[(QUANTUM - 1) * consts::CHANNELS],
                output[QUANTUM * consts::CHANNELS - 1],
            ]);
            source_position += source_frames;
        }
    }

    #[kithara::test]
    #[case::analyse(NativeFault::Analyse)]
    #[case::synthesise(NativeFault::Synthesise)]
    fn native_failure_clears_the_request_and_remains_reusable(
        #[case] fault: NativeFault,
        stretch_pcm: &'static StretchPcm,
    ) {
        const QUANTUM: usize = 64;

        let mut core = anchored_core(stretch_pcm);
        core.native.fail_next(fault);
        let source = signal(stretch_pcm, QUANTUM, consts::CONTEXT_FRAMES * 3);
        let request = ElasticRequest::new(QUANTUM, QUANTUM).expect("unity request");
        let mut output = vec![f32::NAN; QUANTUM * consts::CHANNELS];

        assert!(
            core.render(Some(&source), request, 1.0, Some(&mut output))
                .is_err(),
            "the injected native failure must cross the stream boundary"
        );
        assert!(
            !core.request_pending,
            "no failed request may remain pending"
        );
        assert_eq!(core.samples_needed, 0.0);
        assert!(core.output_chunk.is_none());
        assert!(output.iter().all(|sample| sample.is_nan()));

        let recovery_source = signal(
            stretch_pcm,
            consts::CONTEXT_FRAMES,
            consts::CONTEXT_FRAMES * 3 + QUANTUM,
        );
        let mut recovery_output = vec![0.0; consts::CONTEXT_FRAMES * consts::CHANNELS];
        core.render(
            Some(&recovery_source),
            ElasticRequest::new(consts::CONTEXT_FRAMES, consts::CONTEXT_FRAMES)
                .expect("the recovery request is valid"),
            1.0,
            Some(&mut recovery_output),
        )
        .expect("the explicitly cleared stream accepts a new request");
    }
    #[kithara::test]
    #[case::even_quantum(64)]
    #[case::odd_quantum(257)]
    fn anchored_half_rate_keeps_every_quantized_request_in_its_phase(
        #[case] quantum: usize,
        stretch_pcm: &'static StretchPcm,
    ) {
        use num_traits::ToPrimitive;

        const OUTPUT_FRAMES: usize = 64 * 257 * 8;
        let mut core = anchored_core(stretch_pcm);
        let mut produced = 0usize;
        let mut admitted = 0usize;
        let mut output = vec![f32::NAN; quantum * consts::CHANNELS];
        while produced < OUTPUT_FRAMES {
            let next = produced + quantum;
            let source_frames = next / 2 - produced / 2;
            let source = &stretch_pcm.bungee[..source_frames * consts::CHANNELS];
            core.render(
                Some(source),
                ElasticRequest::new(source_frames, quantum).expect("quantized half-rate request"),
                1.0,
                Some(&mut output),
            )
            .expect("the anchored source renders each exact request");
            produced = next;
            admitted += source_frames;
            assert!(output.iter().all(|sample| sample.is_finite()));
        }
        assert_eq!(admitted, OUTPUT_FRAMES / 2);
        assert_eq!(
            core.audible_source_end,
            u64::try_from(admitted).expect("source count")
        );
        assert_eq!(
            core.audible_output_end,
            u64::try_from(produced).expect("output count")
        );
        let expected = core.grain_output_position * 0.5;
        let phase_error = (core.request.position - expected).abs();
        let quantization_bound = core.max_input_frames().to_f64().expect("native window")
            / quantum.to_f64().expect("quantum")
            + 1.0;
        assert!(
            phase_error <= quantization_bound,
            "native phase accumulated callback rounding: error={phase_error}, bound={quantization_bound}, quantum={quantum}"
        );
    }
}
