use std::mem::size_of;

use ffmpeg::{
    ChannelLayout, Error as FfmpegError,
    codec::encoder::Audio as AudioEncoder,
    format::{Sample, sample::Type as SampleType},
    frame::Audio as AudioFrame,
};
use ffmpeg_next as ffmpeg;

use crate::PcmSource;

pub(crate) fn pump_pcm_frames(
    pcm: &dyn PcmSource,
    chunk_frames: usize,
    mut on_frame: impl FnMut(&AudioFrame) -> Result<(), FfmpegError>,
) -> Result<(), FfmpegError> {
    let bytes_per_frame = pcm.channels() as usize * size_of::<i16>();
    let mut offset = 0;
    let mut pts = 0;
    let total_byte_len = pcm.total_byte_len().unwrap_or(0);

    let mut buf = vec![0u8; chunk_frames * bytes_per_frame];

    while offset < total_byte_len {
        let remaining_bytes = total_byte_len - offset;
        let read_bytes = remaining_bytes.min(buf.len());
        let read = pcm.read_pcm_at(offset, &mut buf[..read_bytes]);
        if read == 0 {
            break;
        }

        let frames_read = read / bytes_per_frame;
        let mut audio_frame = AudioFrame::new(
            Sample::I16(SampleType::Packed),
            frames_read,
            ChannelLayout::default(i32::from(pcm.channels())),
        );
        audio_frame.set_rate(pcm.sample_rate());
        audio_frame.set_pts(Some(pts as i64));
        audio_frame.data_mut(0)[..read].copy_from_slice(&buf[..read]);

        on_frame(&audio_frame)?;

        offset += read;
        pts += frames_read;
    }

    Ok(())
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
    let mut filtered = AudioFrame::empty();
    while filter
        .get("out")
        .ok_or(FfmpegError::Bug)?
        .sink()
        .frame(&mut filtered)
        .is_ok()
    {
        encoder.send_frame(&filtered)?;
        on_packet_drain(encoder)?;
    }

    Ok(())
}
