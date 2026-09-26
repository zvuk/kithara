use std::mem::size_of;

use ffmpeg::{
    ChannelLayout, Error as FfmpegError,
    codec::encoder::Audio as AudioEncoder,
    format::{Sample, sample::Type as SampleType},
    frame::Audio as AudioFrame,
};
use ffmpeg_next as ffmpeg;
use ffmpeg_next::filter::Graph;
use kithara_bufpool::{HasPool, PoolRegion};

use crate::{EncodeError, EncodeResult, PcmSource};

pub(crate) mod consts {
    use super::*;

    pub(crate) const PCM_INPUT_FORMAT: Sample = Sample::I16(SampleType::Packed);
    pub(super) const I16_SCALE: f32 = 32_768.0;
}

fn pump_pcm_bytes<S>(
    pcm: &dyn PcmSource,
    pools: &PoolRegion<S>,
    chunk_frames: usize,
    mut on_frames: impl FnMut(&[u8], usize) -> EncodeResult<()>,
) -> EncodeResult<()>
where
    S: HasPool<u8>,
{
    let bytes_per_frame = usize::from(pcm.channels()) * size_of::<i16>();
    let total_byte_len = pcm.total_byte_len().unwrap_or(0);
    let chunk_bytes = chunk_frames
        .checked_mul(bytes_per_frame)
        .ok_or_else(|| EncodeError::InvalidInput("PCM chunk byte count overflow".to_owned()))?;
    let mut buf = pools.get::<u8>();
    buf.ensure_len(chunk_bytes)
        .map_err(|error| EncodeError::Backend(Box::new(error)))?;
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

pub(crate) fn pump_pcm_samples<S>(
    pcm: &dyn PcmSource,
    pools: &PoolRegion<S>,
    chunk_frames: usize,
    mut on_samples: impl FnMut(&[f32]) -> EncodeResult<()>,
) -> EncodeResult<()>
where
    S: HasPool<u8> + HasPool<f32>,
{
    let mut samples = pools.get::<f32>();

    pump_pcm_bytes(pcm, pools, chunk_frames, |frames, _| {
        samples.clear();
        let sample_count = frames.len() / size_of::<i16>();
        samples
            .ensure_len(sample_count)
            .map_err(|error| EncodeError::Backend(Box::new(error)))?;
        for (sample, pair) in samples
            .iter_mut()
            .zip(frames.chunks_exact(size_of::<i16>()))
        {
            *sample = f32::from(i16::from_le_bytes([pair[0], pair[1]])) / consts::I16_SCALE;
        }
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
    let chunk_bytes = chunk_frames
        .checked_mul(bytes_per_frame)
        .ok_or(FfmpegError::InvalidData)?;
    let total_byte_len = pcm.total_byte_len().unwrap_or(0);
    let mut offset = 0;
    let mut first_frame = 0;

    while offset < total_byte_len {
        let read_bytes = (total_byte_len - offset).min(chunk_bytes);
        let mut audio_frame = AudioFrame::new(
            consts::PCM_INPUT_FORMAT,
            chunk_frames,
            ChannelLayout::default(i32::from(channels)),
        );
        let read = pcm.read_pcm_at(offset, &mut audio_frame.data_mut(0)[..read_bytes]);
        if read == 0 {
            break;
        }

        let frame_bytes = read - read % bytes_per_frame;
        if frame_bytes == 0 {
            break;
        }
        let frames = frame_bytes / bytes_per_frame;
        audio_frame.set_samples(frames);
        audio_frame.set_rate(sample_rate);
        audio_frame.set_pts(Some(first_frame as i64));

        on_frame(&audio_frame)?;

        offset += frame_bytes;
        first_frame += frames;
    }

    Ok(())
}

pub(crate) fn send_frame_to_filter(
    filter: &mut Graph,
    audio_frame: &AudioFrame,
) -> Result<(), FfmpegError> {
    filter
        .get("in")
        .ok_or(FfmpegError::Bug)?
        .source()
        .add(audio_frame)
}

pub(crate) fn flush_filter(filter: &mut Graph) -> Result<(), FfmpegError> {
    filter.get("in").ok_or(FfmpegError::Bug)?.source().flush()
}

pub(crate) fn send_eof_to_encoder(encoder: &mut AudioEncoder) -> Result<(), FfmpegError> {
    encoder.send_eof()
}

pub(crate) fn drain_filtered_frames(
    filter: &mut Graph,
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
    use kithara_test_fixtures::unit_fixtures::{encode_partial_i16, encode_scale_i16};
    use kithara_test_utils::kithara;

    use super::pump_pcm_samples;
    use crate::{test_pcm::TestPcm, test_pools};

    #[kithara::test(native, flash(false))]
    fn i16_input_scales_onto_the_full_scale_f32_range(encode_scale_i16: &'static [u8]) {
        let pcm = TestPcm::from_bytes(encode_scale_i16.to_vec(), 48_000, 1);

        let mut samples = Vec::new();
        pump_pcm_samples(&pcm, &test_pools::pools(), 2, |chunk| {
            samples.extend_from_slice(chunk);
            Ok(())
        })
        .expect("read the source");

        assert_eq!(samples, [-1.0, -0.5, 0.0, 0.5, 32_767.0 / 32_768.0]);
    }

    #[kithara::test(native, flash(false))]
    fn an_incomplete_trailing_frame_is_dropped_at_eof(encode_partial_i16: &'static [u8]) {
        let pcm = TestPcm::from_bytes(encode_partial_i16.to_vec(), 48_000, 2);

        let mut samples = Vec::new();
        pump_pcm_samples(&pcm, &test_pools::pools(), 1, |chunk| {
            samples.extend_from_slice(chunk);
            Ok(())
        })
        .expect("read the source");

        assert_eq!(samples, [0.5, 0.5]);
    }
}
