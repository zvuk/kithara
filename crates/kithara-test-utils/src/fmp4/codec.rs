use kithara_stream::AudioCodec;

use crate::fmp4::bytes::{Mp4Bytes, descriptor, full_box, mp4_box};

pub(crate) struct CodecDescriptor {
    codec: AudioCodec,
}

impl CodecDescriptor {
    pub(crate) fn for_codec(codec: AudioCodec) -> Option<Self> {
        match codec {
            AudioCodec::AacLc => Some(Self {
                codec: AudioCodec::AacLc,
            }),
            AudioCodec::Flac => Some(Self {
                codec: AudioCodec::Flac,
            }),
            _ => None,
        }
    }

    pub(crate) fn sample_entry(
        &self,
        sample_rate: u32,
        channels: u16,
        bit_rate: u64,
        codec_config: &[u8],
    ) -> Vec<u8> {
        match self.codec {
            AudioCodec::AacLc => aac_lc_sample_entry(sample_rate, channels, bit_rate),
            AudioCodec::Flac => flac_sample_entry(sample_rate, channels, codec_config),
            _ => unreachable!("unsupported codec descriptor"),
        }
    }
}

fn aac_lc_sample_entry(sample_rate: u32, channels: u16, bit_rate: u64) -> Vec<u8> {
    mp4_box(*b"mp4a", |buf| {
        buf.push_zeroes(6);
        buf.push_u16(1);
        buf.push_zeroes(8);
        buf.push_u16(channels);
        buf.push_u16(16);
        buf.push_u16(0);
        buf.push_u16(0);
        buf.push_u32(sample_rate << 16);
        buf.push_bytes(&aac_esds(sample_rate, channels, bit_rate));
    })
}

fn aac_esds(sample_rate: u32, channels: u16, bit_rate: u64) -> Vec<u8> {
    let audio_specific_config = aac_audio_specific_config(sample_rate, channels);

    let dec_specific = descriptor(0x05, &audio_specific_config);

    let mut decoder_config = Mp4Bytes::new();
    decoder_config.push_u8(0x40);
    decoder_config.push_u8(0x15);
    decoder_config.push_u24(0);
    decoder_config.push_u32(bit_rate.min(u64::from(u32::MAX)) as u32);
    decoder_config.push_u32(bit_rate.min(u64::from(u32::MAX)) as u32);
    decoder_config.push_bytes(&dec_specific);
    let decoder_config = descriptor(0x04, &decoder_config.into_vec());

    let sl_config = descriptor(0x06, &[0x02]);

    let mut es = Mp4Bytes::new();
    es.push_u16(1);
    es.push_u8(0);
    es.push_bytes(&decoder_config);
    es.push_bytes(&sl_config);
    let es_descriptor = descriptor(0x03, &es.into_vec());

    full_box(*b"esds", 0, 0, |buf| {
        buf.push_bytes(&es_descriptor);
    })
}

fn aac_audio_specific_config(sample_rate: u32, channels: u16) -> [u8; 2] {
    let sample_rate_index = match sample_rate {
        96_000 => 0,
        88_200 => 1,
        64_000 => 2,
        48_000 => 3,
        44_100 => 4,
        32_000 => 5,
        24_000 => 6,
        22_050 => 7,
        16_000 => 8,
        12_000 => 9,
        11_025 => 10,
        8_000 => 11,
        7_350 => 12,
        _ => panic!("unsupported AAC sample rate {sample_rate}"),
    };
    let audio_object_type = 2u8;
    let channel_config = channels as u8;
    [
        (audio_object_type << 3) | (sample_rate_index >> 1),
        ((sample_rate_index & 0x01) << 7) | (channel_config << 3),
    ]
}

fn flac_sample_entry(sample_rate: u32, channels: u16, codec_config: &[u8]) -> Vec<u8> {
    mp4_box(*b"fLaC", |buf| {
        buf.push_zeroes(6);
        buf.push_u16(1);
        buf.push_zeroes(8);
        buf.push_u16(channels);
        buf.push_u16(16);
        buf.push_u16(0);
        buf.push_u16(0);
        buf.push_u32(sample_rate << 16);
        buf.push_bytes(&flac_dfla(codec_config));
    })
}

fn flac_dfla(codec_config: &[u8]) -> Vec<u8> {
    full_box(*b"dfLa", 0, 0, |buf| {
        buf.push_u8(0x80);
        buf.push_u24(codec_config.len() as u32);
        buf.push_bytes(codec_config);
    })
}

#[cfg(test)]
mod tests {
    use kithara_stream::AudioCodec;

    use super::CodecDescriptor;
    use crate::kithara;

    #[kithara::test]
    fn flac_descriptor_emits_flac_sample_entry_and_dfla() {
        let codec_config = vec![0x12; 34];
        let descriptor = CodecDescriptor::for_codec(AudioCodec::Flac).expect("flac descriptor");
        let sample_entry = descriptor.sample_entry(48_000, 2, 0, &codec_config);

        assert!(sample_entry.windows(4).any(|window| window == b"fLaC"));
        assert!(sample_entry.windows(4).any(|window| window == b"dfLa"));
        assert!(sample_entry.ends_with(&codec_config));
    }
}
