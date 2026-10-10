#![cfg(feature = "render")]

use std::num::NonZeroU32;
#[cfg(feature = "stretch-identity")]
use std::num::NonZeroUsize;

use kithara_signal::{AudioChunk, AudioChunkInfo, AudioSpec};
use kithara_test_fixtures::unit_fixtures::warp_sine;
use kithara_test_utils::{
    bufpool::{pools, sample_buffer},
    kithara,
};
#[cfg(feature = "stretch-identity")]
use kithara_warp::{SpeedCurve, StretchKind, WarpCapabilities};
use kithara_warp::{Warp, WarpConfig};

#[cfg(feature = "stretch-identity")]
trait TerminalDrain {
    fn flush(&mut self) -> Option<AudioChunk>;
}

#[cfg(feature = "stretch-identity")]
impl<S: kithara_bufpool::HasPool<f32>> TerminalDrain for kithara_warp::WarpRenderer<S> {
    fn flush(&mut self) -> Option<AudioChunk> {
        self.drain(usize::MAX).expect("terminal drain")
    }
}

#[cfg(feature = "stretch-identity")]
#[kithara::test]
fn identity_preserves_pcm_and_unity_accounting_despite_live_controls() {
    let bits = [
        0x0000_0000,
        0x8000_0000,
        0x7fc0_1234,
        0xffc0_5678,
        0x7f7f_ffff,
        0xff7f_ffff,
        0x0000_0001,
        0x8000_0001,
        0x7f80_0000,
        0xff80_0000,
        0x3f80_0000,
        0xbf80_0000,
        0x0080_0000,
        0x8080_0000,
        0x3e80_0000,
        0xbe80_0000,
    ];
    let samples = bits.map(f32::from_bits);
    let spec = AudioSpec::new(2, NonZeroU32::new(44_100).expect("sample rate"));
    let pools = pools();
    let config = WarpConfig::builder()
        .speed(0.5)
        .backend(StretchKind::Identity)
        .keylock(true)
        .render_quantum_frames(NonZeroUsize::new(8).expect("quantum"))
        .build();
    let mut renderer = Warp::new((), &config).renderer(spec, pools.clone());

    assert!(StretchKind::Identity.capabilities().is_empty());
    assert!(
        !StretchKind::Identity
            .capabilities()
            .contains(WarpCapabilities::RATE)
    );
    assert!(
        !StretchKind::Identity
            .capabilities()
            .contains(WarpCapabilities::KEYLOCK)
    );
    assert!(!renderer.requires_staging());
    assert!(renderer.accepts_input());

    for (index, (speed, keylock)) in [(0.5, true), (3.75, false), (0.25, true)]
        .into_iter()
        .enumerate()
    {
        let revision = u64::try_from(index).expect("revision");
        renderer
            .set_speed(SpeedCurve::Constant(speed), revision)
            .expect("valid speed");
        renderer.set_keylock(keylock);
        renderer.prepare(spec);
        let input = AudioChunk::new(
            AudioChunkInfo {
                spec,
                frames: 8,
                frame_offset: revision * 8,
                ..AudioChunkInfo::default()
            },
            sample_buffer(&pools, &samples),
        );
        let original_samples = input.samples.as_ptr();
        let frames = renderer
            .prepare_quantum(input.meta, 8, usize::MAX)
            .expect("Identity admits the unchanged source span");
        assert_eq!(frames.get(), 8);
        let output = renderer
            .render_quantum(input)
            .continue_value()
            .expect("the prepared source span is accepted")
            .expect("Identity emits the original PCM");
        assert_eq!(output.samples.as_ptr(), original_samples);
        assert_eq!(
            output
                .samples
                .iter()
                .map(|sample| sample.to_bits())
                .collect::<Vec<_>>(),
            bits
        );
        assert_eq!(output.spec(), spec);
        assert_eq!(output.frames(), 8);
        assert_eq!(output.meta.frame_offset, revision * 8);
        assert_eq!(output.meta.render_revision, revision);
        assert_eq!(
            renderer.rendered_source_end(),
            Some(((revision + 1) * 8, spec.sample_rate))
        );
        assert!(!renderer.transition_pending());
        renderer.prepare(spec);
        assert!(renderer.flush().is_none(), "Identity owns no terminal tail");
    }
}

#[kithara::test]
fn warp_unity_preserves_original_samples(warp_sine: Vec<f32>) {
    let spec = AudioSpec::new(2, NonZeroU32::new(44_100).expect("fixture sample rate"));
    let pools = pools();
    let config = WarpConfig::builder().build();
    let mut renderer = Warp::new((), &config).renderer(spec, pools.clone());
    let source = &warp_sine[..4096 * 2];
    let input = AudioChunk::new(
        AudioChunkInfo {
            spec,
            frames: 4096,
            ..AudioChunkInfo::default()
        },
        sample_buffer(&pools, source),
    );
    let original_samples = input.samples.as_ptr();

    renderer.prepare(spec);
    let output = renderer
        .render(input)
        .continue_value()
        .expect("the complete source span is accepted")
        .expect("unity rendering produces samples");

    assert_eq!(output.samples.as_ptr(), original_samples);
    assert_eq!(output.samples.as_ref(), source);
    assert_eq!(output.spec(), spec);
    assert_eq!(output.frames(), 4096);
}
