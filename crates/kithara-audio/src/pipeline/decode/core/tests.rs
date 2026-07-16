use std::num::{NonZeroU32, NonZeroUsize};

use kithara_bufpool::PcmPool;
use kithara_decode::{
    DecodeResult, Decoder, DecoderChunkOutcome, DecoderSeekOutcome, GaplessMode, PcmChunk, PcmMeta,
    PcmSpec,
};
use kithara_platform::{CancelToken, time::Duration};
use kithara_test_utils::kithara;

use super::{DecodeCore, DecoderSession, GaplessStep};
use crate::{
    pipeline::gapless::GaplessStage,
    renderer::AudioWorkerHandle,
    source_audio::{SourceAudioReadOutcome, SourceFrameRange, connect_source_audio},
};

struct TestDecoder(PcmSpec);

impl Decoder for TestDecoder {
    fn duration(&self) -> Option<Duration> {
        None
    }

    fn next_chunk(&mut self) -> DecodeResult<DecoderChunkOutcome> {
        Ok(DecoderChunkOutcome::Eof)
    }

    fn seek(&mut self, _position: Duration) -> DecodeResult<DecoderSeekOutcome> {
        Ok(DecoderSeekOutcome::PastEof {
            duration: Duration::ZERO,
        })
    }

    fn spec(&self) -> PcmSpec {
        self.0
    }

    fn update_byte_len(&self, _len: u64) {}
}

fn chunk(pool: &PcmPool, spec: PcmSpec, offset: u64, samples: [f32; 2]) -> PcmChunk {
    PcmChunk::new(
        PcmMeta {
            spec,
            frames: 2,
            frame_offset: offset,
            ..Default::default()
        },
        pool.attach(samples.to_vec()),
    )
}

#[kithara::test]
fn authoritative_demand_boundary_parks_next_decoder_window_without_processed_output() {
    let spec = PcmSpec::new(1, NonZeroU32::new(48_000).expect("static sample rate"));
    let pool = PcmPool::default();
    let worker = AudioWorkerHandle::with_cancel(CancelToken::never());
    let (mut reader, mut tap) = connect_source_audio(
        &pool,
        NonZeroUsize::new(2).expect("non-zero source capacity"),
        NonZeroUsize::new(2).expect("non-zero source window"),
        worker.clone(),
    )
    .expect("source audio connection");
    reader
        .activate_authoritative(spec)
        .expect("authoritative activation");
    tap.service().expect("install activation");
    let first_demand = reader
        .request(
            SourceFrameRange::new(0, 2).expect("first source range"),
            0,
            7,
        )
        .expect("first source demand");
    tap.service().expect("install first demand");

    let decoder: Box<dyn Decoder> = Box::new(TestDecoder(spec));
    let gapless = GaplessStage::build(decoder.as_ref(), GaplessMode::Disabled, None);
    let mut core = DecodeCore::new(
        DecoderSession {
            decoder,
            media_info: None,
            base_offset: 0,
            installed_at_seek_epoch: 7,
        },
        GaplessMode::Disabled,
        gapless,
        Vec::new(),
        Some(tap),
    );

    core.push(chunk(&pool, spec, 0, [1.0, 2.0]));
    assert!(matches!(
        core.next_gapless(7),
        Ok(GaplessStep::SourceProgress)
    ));
    let mut first_output = [0.0; 2];
    assert_eq!(
        reader.read_into(&first_demand, &mut first_output),
        Ok(SourceAudioReadOutcome::Ready { frames: 2 })
    );
    assert_eq!(first_output, [1.0, 2.0]);

    core.push(chunk(&pool, spec, 2, [3.0, 4.0]));
    assert!(matches!(
        core.next_gapless(7),
        Ok(GaplessStep::SourceProgress)
    ));
    assert!(!core.source_audio_ready());

    let second_demand = reader
        .request(
            SourceFrameRange::new(2, 4).expect("second source range"),
            0,
            7,
        )
        .expect("second source demand");
    core.flush_reader_signals();
    assert!(core.source_audio_ready());
    assert!(matches!(
        core.next_gapless(7),
        Ok(GaplessStep::SourceProgress)
    ));
    let mut second_output = [0.0; 2];
    assert_eq!(
        reader.read_into(&second_demand, &mut second_output),
        Ok(SourceAudioReadOutcome::Ready { frames: 2 })
    );
    assert_eq!(second_output, [3.0, 4.0]);
    assert!(matches!(core.next_gapless(7), Ok(GaplessStep::Empty)));
    worker.shutdown();
}
