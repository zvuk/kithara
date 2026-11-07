use std::sync::OnceLock;

use ffmpeg::{
    ChannelLayout, Codec, Error as FfmpegError, Packet, Rational,
    codec::{
        Id,
        capabilities::Capabilities,
        encoder::{Audio as AudioEncoder, find_by_name},
    },
    error::EAGAIN,
    filter::{self, Graph as FilterGraph},
    format as av_format,
    rescale::Rescale,
};
use ffmpeg_next as ffmpeg;

use crate::{EncodeError, types::EncodedAccessUnit};

#[derive(Clone, Copy)]
pub(crate) struct RebaseRates {
    pub(crate) encoder: Rational,
    pub(crate) target: Rational,
}

#[derive(Clone, Copy)]
pub(crate) enum PacketCodec {
    Aac,
    Flac,
}

pub(crate) fn collect_encoded_packets(
    encoder: &mut AudioEncoder,
    rates: RebaseRates,
    timestamp_origin: &mut Option<i64>,
    units: &mut Vec<EncodedAccessUnit>,
    codec: PacketCodec,
) -> Result<(), FfmpegError> {
    let mut encoded = Packet::empty();
    loop {
        match encoder.receive_packet(&mut encoded) {
            Ok(()) => {}
            Err(FfmpegError::Eof | FfmpegError::Other { errno: EAGAIN }) => return Ok(()),
            Err(error) => return Err(error),
        }
        if encoded.size() == 0 {
            continue;
        }

        let raw_pts = encoded.pts().unwrap_or_default();
        let raw_dts = encoded.dts().unwrap_or_default();
        let origin = *timestamp_origin.get_or_insert(raw_pts.min(raw_dts));
        let start = raw_pts.saturating_sub(origin).max(0);
        let end = start.saturating_add(encoded.duration().max(0));
        let pts = rescale_timestamp(start, rates);

        units.push(EncodedAccessUnit {
            bytes: encoded.data().unwrap_or(&[]).to_vec(),
            pts,
            dts: rescale_timestamp(raw_dts.saturating_sub(origin).max(0), rates),
            duration: packet_duration(end, pts, rates, codec),
            is_sync: encoded.is_key(),
        });
    }
}

fn packet_duration(end: i64, pts: u64, rates: RebaseRates, codec: PacketCodec) -> u32 {
    let duration = rescale_timestamp(end, rates).saturating_sub(pts);
    u32::try_from(duration).unwrap_or_else(|_| {
        match codec {
            PacketCodec::Aac => tracing::error!(
                packet_duration = duration,
                "BUG: AAC packet duration exceeds u32::MAX in the target time base"
            ),
            PacketCodec::Flac => tracing::error!(
                packet_duration = duration,
                "BUG: FLAC packet duration exceeds u32::MAX in the target time base"
            ),
        }
        0
    })
}

fn rescale_timestamp(value: i64, rates: RebaseRates) -> u64 {
    let rescaled = value.rescale(rates.encoder, rates.target).max(0);
    u64::try_from(rescaled).unwrap_or_else(|_| {
        tracing::error!(rescaled, "BUG: rescaled timestamp exceeds u64::MAX");
        0
    })
}

pub(crate) fn ensure_ffmpeg_initialized() -> Result<(), EncodeError> {
    static INIT: OnceLock<Result<(), String>> = OnceLock::new();

    match INIT.get_or_init(|| ffmpeg::init().map_err(|error| error.to_string())) {
        Ok(()) => Ok(()),
        Err(message) => Err(EncodeError::backend_message(message.clone())),
    }
}

/// The encoder this crate writes `codec` with. A codec id alone leaves the
/// pick to whichever encoders the `FFmpeg` build registers, and two encoders
/// of one codec write different packet geometry. `FFmpeg`'s native encoder
/// carries its codec's name; MP3 has only the LAME library encoder, and Opus
/// is written by libopus.
pub(crate) fn find_encoder(codec: Id) -> Option<Codec> {
    find_by_name(match codec {
        Id::MP3 => "libmp3lame",
        Id::OPUS => "libopus",
        _ => codec.name(),
    })
}

pub(crate) fn build_direct_filter(
    encoder: &AudioEncoder,
    sample_rate: u32,
    channels: u16,
    input_format: av_format::Sample,
) -> Result<FilterGraph, FfmpegError> {
    let mut graph = FilterGraph::new();
    let input_channel_layout = ChannelLayout::default(i32::from(channels));
    let args = format!(
        "time_base=1/{}:sample_rate={}:sample_fmt={}:channel_layout=0x{:x}",
        sample_rate,
        sample_rate,
        input_format.name(),
        input_channel_layout.bits()
    );

    graph.add(
        &filter::find("abuffer").ok_or(FfmpegError::Bug)?,
        "in",
        &args,
    )?;
    graph.add(
        &filter::find("abuffersink").ok_or(FfmpegError::Bug)?,
        "out",
        "",
    )?;

    let aformat_args = format!(
        "aformat=sample_fmts={}:sample_rates={}:channel_layouts=0x{:x}",
        encoder.format().name(),
        encoder.rate(),
        encoder.channel_layout().bits()
    );
    graph
        .output("in", 0)?
        .input("out", 0)?
        .parse(&aformat_args)?;
    graph.validate()?;

    if let Some(codec) = encoder.codec()
        && !codec
            .capabilities()
            .contains(Capabilities::VARIABLE_FRAME_SIZE)
    {
        graph
            .get("out")
            .ok_or(FfmpegError::Bug)?
            .sink()
            .set_frame_size(encoder.frame_size());
    }

    Ok(graph)
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::{Id, ensure_ffmpeg_initialized, find_encoder};

    /// A codec with several encoders is written by the one its fixtures name,
    /// not by whichever the `FFmpeg` build registers first.
    #[kithara::test]
    #[case::vorbis(Id::VORBIS, "vorbis")]
    #[case::mp3(Id::MP3, "libmp3lame")]
    #[case::opus(Id::OPUS, "libopus")]
    #[case::aac(Id::AAC, "aac")]
    fn a_codec_is_written_by_the_encoder_its_fixtures_name(
        #[case] codec: Id,
        #[case] encoder: &str,
    ) {
        ensure_ffmpeg_initialized().expect("FFmpeg initializes");
        let found = find_encoder(codec).expect("FFmpeg registers an encoder for the codec");
        assert_eq!(found.name(), encoder);
    }
}
