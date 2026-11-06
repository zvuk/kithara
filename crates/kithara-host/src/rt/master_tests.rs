use firewheel::{
    clock::InstantSamples,
    dsp::{buffer::ConstSequentialBuffer, declick::DeclickValues},
    log::{RealtimeLoggerConfig, realtime_logger},
    mask::{ConnectedMask, ConstantMask, SilenceMask},
    node::{NUM_SCRATCH_BUFFERS, ProcStore, StreamStatus},
};
use kithara_command::scoped_channel;
use kithara_config::Config;
use kithara_platform::time::Duration;
use kithara_test_utils::bufpool::TestPools;
use kithara_warp::BeatGridId;

use super::*;
use crate::{
    host::HostSettings,
    session::transport::{SessionGridGeneration, TransportState, ducking_store},
};

fn info(frames: usize) -> ProcInfo {
    ProcInfo {
        sample_rate: NonZeroU32::new(48_000).expect("rate"),
        frames,
        in_silence_mask: SilenceMask::NONE_SILENT,
        out_silence_mask: SilenceMask::NONE_SILENT,
        in_constant_mask: ConstantMask::default(),
        out_constant_mask: ConstantMask::default(),
        in_connected_mask: ConnectedMask::default(),
        out_connected_mask: ConnectedMask::default(),
        total_cpu_seconds_recip: 1.0,
        process_to_playback_delay: None,
        did_just_unbypass: false,
        last_marker_instant: InstantSamples(0),
        sample_rate_recip: 1.0 / 48_000.0,
        clock_samples: InstantSamples(0),
        duration_since_stream_start: Duration::ZERO,
        stream_status: StreamStatus::empty(),
        dropped_frames: 0,
    }
}

fn extra(ducking: Option<SessionDuckingMode>) -> ProcExtra {
    let (logger, _receiver) = realtime_logger(RealtimeLoggerConfig::default());
    let mut store = ProcStore::with_capacity(1);
    if let Some(ducking) = ducking {
        let config = crate::HostConfig::<TestPools>::builder()
            .build()
            .channel_config();
        let (_sender, inbox) = scoped_channel(config);
        let grid = SessionGridGeneration::new(BeatGridId::allocate().expect("grid"));
        assert!(
            store
                .insert(TransportState::new(
                    inbox,
                    HostSettings::builder().ducking(ducking).build(),
                    grid,
                    config.values().root.values().capacity.get(),
                ))
                .is_ok()
        );
    }
    ProcExtra {
        logger,
        store,
        scratch_buffers: ConstSequentialBuffer::<f32, NUM_SCRATCH_BUFFERS>::new(1024),
        declick_values: DeclickValues::new(NonZeroU32::new(16).expect("declick")),
    }
}

#[kithara::test]
fn master_ducking_pcm_and_terminal_smoothing_match_the_scalar_path() {
    for mode in [SessionDuckingMode::Soft, SessionDuckingMode::Hard] {
        let mut extra = extra(Some(mode));
        for frames in [0, 1, 255, 256, 257, 513] {
            for initial in [1.0, mode.volume().amp() + 0.000_1] {
                let info = info(frames);
                let mut processor = MasterProcessor {
                    sample_rate: info.sample_rate,
                    gain: None,
                };
                let gain = processor.gain(mode);
                gain.set_value(initial);
                gain.reset_to_target();
                let mut expected_gain = *gain;
                expected_gain.set_value(mode.volume().amp());
                if frames > consts::GAIN_CHUNK && initial != 1.0 {
                    let mut premature = expected_gain;
                    premature.process_into_buffer(&mut [0.0; consts::GAIN_CHUNK]);
                    assert!(premature.has_settled());
                    assert!(!expected_gain.has_settled());
                }
                let left: Vec<_> = [0.25, -0.75, 0.0, -0.0]
                    .into_iter()
                    .cycle()
                    .take(frames + 3)
                    .collect();
                let right = vec![-0.5; frames + 2];
                let mut output_left = vec![-0.0_f32; frames + 3];
                let mut output_right = vec![0.75_f32; frames + 2];
                let (mut expected_left, mut expected_right) =
                    (output_left.clone(), output_right.clone());
                for (((l, r), x), y) in expected_left[..frames]
                    .iter_mut()
                    .zip(&mut expected_right[..frames])
                    .zip(&left)
                    .zip(&right)
                {
                    let gain = expected_gain.next_smoothed();
                    *l = x * gain;
                    *r = y * gain;
                }
                expected_gain.settle();
                let status = processor.process(
                    &info,
                    ProcBuffers {
                        inputs: &[&left, &right],
                        outputs: &mut [&mut output_left, &mut output_right],
                    },
                    &mut extra,
                );
                assert_eq!(status, ProcessStatus::OutputsModified);
                assert_eq!(
                    output_left.iter().map(|x| x.to_bits()).collect::<Vec<_>>(),
                    expected_left
                        .iter()
                        .map(|x| x.to_bits())
                        .collect::<Vec<_>>()
                );
                assert_eq!(
                    output_right.iter().map(|x| x.to_bits()).collect::<Vec<_>>(),
                    expected_right
                        .iter()
                        .map(|x| x.to_bits())
                        .collect::<Vec<_>>()
                );
                assert_eq!(processor.gain, Some(expected_gain));
            }
        }
    }
}

#[kithara::test]
fn master_ducking_preserves_multispan_pcm_and_invalid_range_state() {
    let changes = [
        (0, SessionDuckingMode::Soft),
        (257, SessionDuckingMode::Hard),
        (513, SessionDuckingMode::Off),
        (769, SessionDuckingMode::Soft),
    ];
    for frames in [0, 1, 256, 257, 513, 769, 1023] {
        for initial in [1.0, SessionDuckingMode::Soft.volume().amp() + 0.000_1] {
            let mut extra = extra(None);
            extra.store = ducking_store(&changes, 1023);
            assert_eq!(
                applied_spans(&extra.store, frames).expect("spans").count(),
                4
            );
            let info = info(frames);
            let mut processor = MasterProcessor {
                sample_rate: info.sample_rate,
                gain: None,
            };
            let gain = processor.gain(SessionDuckingMode::Soft);
            gain.set_value(initial);
            gain.reset_to_target();
            let mut expected_gain = *gain;
            let left: Vec<_> = [0.25_f32, -0.75, 0.0, -0.0]
                .into_iter()
                .cycle()
                .take(frames + 3)
                .collect();
            let right = vec![-0.5; frames + 2];
            let mut output_left = vec![-0.0_f32; frames + 3];
            let mut output_right = vec![0.75_f32; frames + 2];
            let (mut expected_left, mut expected_right) =
                (output_left.clone(), output_right.clone());
            let mut invalid = 0;
            for (range, span) in applied_spans(&extra.store, frames).expect("spans") {
                expected_gain.set_value(span.settings().ducking().volume().amp());
                let (Some(left), Some(right), Some(out_left), Some(out_right)) = (
                    left[..frames].get(range.clone()),
                    right[..frames].get(range.clone()),
                    expected_left[..frames].get_mut(range.clone()),
                    expected_right[..frames].get_mut(range),
                ) else {
                    invalid += 1;
                    continue;
                };
                for (((out_left, out_right), left), right) in
                    out_left.iter_mut().zip(out_right).zip(left).zip(right)
                {
                    let gain = expected_gain.next_smoothed();
                    *out_left = left * gain;
                    *out_right = right * gain;
                }
            }
            if frames < 769 {
                assert!(invalid > 0);
            }
            expected_gain.settle();
            assert_eq!(
                processor.process(
                    &info,
                    ProcBuffers {
                        inputs: &[&left, &right],
                        outputs: &mut [&mut output_left, &mut output_right],
                    },
                    &mut extra,
                ),
                ProcessStatus::OutputsModified
            );
            assert_eq!(
                output_left.iter().map(|x| x.to_bits()).collect::<Vec<_>>(),
                expected_left
                    .iter()
                    .map(|x| x.to_bits())
                    .collect::<Vec<_>>()
            );
            assert_eq!(
                output_right.iter().map(|x| x.to_bits()).collect::<Vec<_>>(),
                expected_right
                    .iter()
                    .map(|x| x.to_bits())
                    .collect::<Vec<_>>()
            );
            assert_eq!(processor.gain, Some(expected_gain));
        }
    }
}

#[kithara::test]
fn master_ducking_leaves_short_input_and_output_buffers_untouched() {
    for (input_frames, output_frames) in [(2, 3), (3, 2)] {
        let info = info(3);
        let mut extra = extra(Some(SessionDuckingMode::Soft));
        let mut processor = MasterProcessor {
            sample_rate: info.sample_rate,
            gain: None,
        };
        let expected_gain = *processor.gain(SessionDuckingMode::Soft);
        let left = vec![0.5; input_frames];
        let right = [0.75; 3];
        let mut out_left = vec![-0.0_f32; output_frames];
        let mut out_right = [-0.25; 3];
        assert_eq!(
            processor.process(
                &info,
                ProcBuffers {
                    inputs: &[&left, &right],
                    outputs: &mut [&mut out_left, &mut out_right],
                },
                &mut extra,
            ),
            ProcessStatus::Bypass
        );
        assert!(out_left.iter().all(|x| x.to_bits() == (-0.0_f32).to_bits()));
        assert_eq!(out_right, [-0.25; 3]);
        assert_eq!(processor.gain, Some(expected_gain));
    }
}

#[kithara::test]
fn master_ducking_retains_silence_bypass_missing_buffers_and_rate_changes() {
    let mut processor = MasterProcessor {
        sample_rate: info(1).sample_rate,
        gain: None,
    };
    let mut output = [0.25_f32; 3];
    for (mode, silent, expected) in [
        (None, false, ProcessStatus::Bypass),
        (Some(SessionDuckingMode::Off), false, ProcessStatus::Bypass),
        (
            Some(SessionDuckingMode::Hard),
            true,
            ProcessStatus::ClearAllOutputs,
        ),
        (Some(SessionDuckingMode::Soft), false, ProcessStatus::Bypass),
    ] {
        let mut extra = extra(mode);
        let mut info = info(3);
        if silent {
            info.in_silence_mask = SilenceMask::STEREO_SILENT;
        }
        let status = processor.process(
            &info,
            ProcBuffers {
                inputs: &[],
                outputs: &mut [&mut output],
            },
            &mut extra,
        );
        assert_eq!(status, expected);
        assert_eq!(output, [0.25; 3]);
        if silent {
            assert!(
                processor
                    .gain
                    .expect("gain")
                    .has_settled_at(SessionDuckingMode::Hard.volume().amp())
            );
        }
    }
    let mut extra = extra(Some(SessionDuckingMode::Soft));
    let mut expected = processor.gain.expect("gain");
    let stream = StreamInfo {
        sample_rate: NonZeroU32::new(96_000).expect("rate"),
        ..Default::default()
    };
    expected.update_sample_rate(stream.sample_rate);
    processor.new_stream(
        &stream,
        &mut ProcStreamCtx {
            store: &mut extra.store,
            logger: &mut extra.logger,
        },
    );
    assert_eq!(processor.sample_rate, stream.sample_rate);
    assert_eq!(processor.gain, Some(expected));
}
