use std::mem::size_of;

use ffmpeg::{
    ChannelLayout, Error as FfmpegError,
    codec::encoder::Audio as AudioEncoder,
    format::{Sample, sample::Type as SampleType},
    frame::Audio as AudioFrame,
};
use ffmpeg_next as ffmpeg;

use crate::{EncodeResult, PcmSource};

/// Sample format of the interleaved i16 bytes [`PcmSource`] yields.
pub(crate) const PCM_INPUT_FORMAT: Sample = Sample::I16(SampleType::Packed);

const I16_SCALE: f32 = 32_768.0;

/// Read the source in `chunk_frames` steps and hand over whole frames only,
/// together with the index of the first frame in the chunk.
fn pump_pcm_bytes<E>(
    pcm: &dyn PcmSource,
    chunk_frames: usize,
    mut on_frames: impl FnMut(&[u8], usize) -> Result<(), E>,
) -> Result<(), E> {
    let bytes_per_frame = usize::from(pcm.channels()) * size_of::<i16>();
    let total_byte_len = pcm.total_byte_len().unwrap_or(0);

    let mut buf = vec![0u8; chunk_frames * bytes_per_frame];
    let mut offset = 0;
    let mut first_frame = 0;

    while offset < total_byte_len {
        let remaining_bytes = total_byte_len - offset;
        let read_bytes = remaining_bytes.min(buf.len());
        let read = pcm.read_pcm_at(offset, &mut buf[..read_bytes]);
        if read == 0 {
            break;
        }

        let frame_bytes = read - read % bytes_per_frame;
        if frame_bytes == 0 {
            break;
        }

        on_frames(&buf[..frame_bytes], first_frame)?;

        offset += frame_bytes;
        first_frame += frame_bytes / bytes_per_frame;
    }

    Ok(())
}

/// Hand each chunk of the source over as interleaved f32.
pub(crate) fn pump_pcm_samples(
    pcm: &dyn PcmSource,
    chunk_frames: usize,
    mut on_samples: impl FnMut(&[f32]) -> EncodeResult<()>,
) -> EncodeResult<()> {
    let mut samples = Vec::with_capacity(chunk_frames * usize::from(pcm.channels()));

    pump_pcm_bytes(pcm, chunk_frames, |frames, _| {
        samples.clear();
        samples.extend(
            frames
                .chunks_exact(size_of::<i16>())
                .map(|pair| f32::from(i16::from_le_bytes([pair[0], pair[1]])) / I16_SCALE),
        );
        on_samples(&samples)
    })
}

pub(crate) fn pump_pcm_frames(
    pcm: &dyn PcmSource,
    chunk_frames: usize,
    mut on_frame: impl FnMut(&AudioFrame) -> Result<(), FfmpegError>,
) -> Result<(), FfmpegError> {
    let channels = pcm.channels();
    let sample_rate = pcm.sample_rate();
    let bytes_per_frame = usize::from(channels) * size_of::<i16>();

    pump_pcm_bytes(pcm, chunk_frames, |frames, first_frame| {
        let mut audio_frame = AudioFrame::new(
            PCM_INPUT_FORMAT,
            frames.len() / bytes_per_frame,
            ChannelLayout::default(i32::from(channels)),
        );
        audio_frame.set_rate(sample_rate);
        audio_frame.set_pts(Some(first_frame as i64));
        audio_frame.data_mut(0)[..frames.len()].copy_from_slice(frames);

        on_frame(&audio_frame)
    })
}

pub(crate) fn send_frame_to_filter(
    filter: &mut ffmpeg::filter::Graph,
    audio_frame: &AudioFrame,
) -> Result<(), FfmpegError> {
    filter
        .get("in")
        .ok_or(FfmpegError::Bug)?
        .source()
        .add(audio_frame)
}

pub(crate) fn flush_filter(filter: &mut ffmpeg::filter::Graph) -> Result<(), FfmpegError> {
    filter.get("in").ok_or(FfmpegError::Bug)?.source().flush()
}

pub(crate) fn send_eof_to_encoder(encoder: &mut AudioEncoder) -> Result<(), FfmpegError> {
    encoder.send_eof()
}

pub(crate) fn drain_filtered_frames(
    filter: &mut ffmpeg::filter::Graph,
    encoder: &mut AudioEncoder,
    mut on_packet_drain: impl FnMut(&mut AudioEncoder) -> Result<(), FfmpegError>,
) -> Result<(), FfmpegError> {
    loop {
        let mut filtered = AudioFrame::empty();
        if filter
            .get("out")
            .ok_or(FfmpegError::Bug)?
            .sink()
            .frame(&mut filtered)
            .is_err()
        {
            return Ok(());
        }

        encoder.send_frame(&filtered)?;
        on_packet_drain(encoder)?;
    }
}

#[cfg(test)]
mod tests {
    use super::pump_pcm_samples;
    use crate::test_pcm::TestPcm;

    #[test]
    fn i16_input_scales_onto_the_full_scale_f32_range() {
        let pcm = TestPcm::from_samples(&[i16::MIN, -16_384, 0, 16_384, i16::MAX], 48_000, 1);

        let mut samples = Vec::new();
        pump_pcm_samples(&pcm, 2, |chunk| {
            samples.extend_from_slice(chunk);
            Ok(())
        })
        .expect("read the source");

        assert_eq!(samples, [-1.0, -0.5, 0.0, 0.5, 32_767.0 / 32_768.0]);
    }

    #[test]
    fn an_incomplete_trailing_frame_is_dropped_at_eof() {
        let pcm = TestPcm::from_bytes(vec![0x00, 0x40, 0x00, 0x40, 0x11], 48_000, 2);

        let mut samples = Vec::new();
        pump_pcm_samples(&pcm, 1, |chunk| {
            samples.extend_from_slice(chunk);
            Ok(())
        })
        .expect("read the source");

        assert_eq!(samples, [0.5, 0.5]);
    }
}
