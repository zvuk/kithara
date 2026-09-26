use super::Wave;

mod consts {
    pub(super) const BITS_PER_SAMPLE: u16 = 16;
    pub(super) const FMT_CHUNK_BYTES: u32 = 16;
    pub(super) const HEADER_BYTES: usize = 44;
    pub(super) const PCM_FORMAT_TAG: u16 = 1;
    pub(super) const RIFF_PRELUDE_BYTES: usize = 8;
    pub(super) const SAMPLE_BYTES: usize = 2;
    /// Size fields of a streaming header, which names no total.
    pub(super) const UNKNOWN_SIZE: u32 = 0xFFFF_FFFF;
}

/// The 44-byte RIFF/WAVE header for interleaved 16-bit PCM.
///
/// `data_bytes` is `None` for a streaming header, whose two size fields say
/// `0xFFFFFFFF` because the total is not known when the header is written.
#[must_use]
pub fn header(sample_rate: u32, channels: u16, data_bytes: Option<usize>) -> Vec<u8> {
    let mut out = Vec::with_capacity(consts::HEADER_BYTES);
    write_header(&mut out, sample_rate, channels, data_bytes);
    out
}

/// Interleaved 16-bit RIFF/WAVE carrying a waveform, sized to fit `total_bytes`
/// header included.
///
/// # Panics
///
/// Panics when `total_bytes` leaves no room for the header.
#[must_use]
pub fn wav_of_size(sample_rate: u32, channels: u16, total_bytes: usize, wave: Wave) -> Vec<u8> {
    let payload = total_bytes
        .checked_sub(consts::HEADER_BYTES)
        .expect("a WAV of a given size has room for its 44-byte header");
    let frames = payload / (usize::from(channels) * consts::SAMPLE_BYTES);
    wav(sample_rate, channels, frames, wave)
}

/// Interleaved 16-bit RIFF/WAVE carrying a waveform.
#[must_use]
pub fn wav(sample_rate: u32, channels: u16, total_frames: usize, wave: Wave) -> Vec<u8> {
    wav_from_fn(sample_rate, channels, total_frames, |frame| {
        wave.sample(frame, sample_rate)
    })
}

/// Interleaved 16-bit RIFF/WAVE around a per-frame sample function, for a body
/// no single [`Wave`] describes.
#[must_use]
pub fn wav_from_fn<S: Fn(usize) -> i16>(
    sample_rate: u32,
    channels: u16,
    total_frames: usize,
    sample: S,
) -> Vec<u8> {
    let data_bytes = total_frames * usize::from(channels) * consts::SAMPLE_BYTES;
    let mut out = Vec::with_capacity(consts::HEADER_BYTES + data_bytes);
    write_header(&mut out, sample_rate, channels, Some(data_bytes));

    for frame in 0..total_frames {
        let value = sample(frame).to_le_bytes();
        for _ in 0..usize::from(channels) {
            out.extend_from_slice(&value);
        }
    }
    out
}

fn write_header(out: &mut Vec<u8>, sample_rate: u32, channels: u16, data_bytes: Option<usize>) {
    let block_align =
        channels * u16::try_from(consts::SAMPLE_BYTES).expect("invariant: 2 fits u16");
    let byte_rate = sample_rate * u32::from(block_align);
    let (riff_bytes, data_field) =
        data_bytes.map_or((consts::UNKNOWN_SIZE, consts::UNKNOWN_SIZE), |data_bytes| {
            let data_field =
                u32::try_from(data_bytes).expect("invariant: a fixture WAV payload fits u32");
            let riff_bytes =
                u32::try_from(consts::HEADER_BYTES - consts::RIFF_PRELUDE_BYTES + data_bytes)
                    .expect("invariant: a fixture WAV fits u32");
            (riff_bytes, data_field)
        });

    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&riff_bytes.to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&consts::FMT_CHUNK_BYTES.to_le_bytes());
    out.extend_from_slice(&consts::PCM_FORMAT_TAG.to_le_bytes());
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&consts::BITS_PER_SAMPLE.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_field.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    fn field(bytes: &[u8], at: usize) -> u32 {
        u32::from_le_bytes(bytes[at..at + 4].try_into().expect("a four-byte field"))
    }

    #[kithara::test(native, flash(false))]
    fn a_header_names_its_riff_chunks() {
        let head = header(44_100, 2, Some(0));

        assert_eq!(head.len(), consts::HEADER_BYTES);
        assert_eq!(&head[0..4], b"RIFF");
        assert_eq!(&head[8..12], b"WAVE");
        assert_eq!(&head[36..40], b"data");
    }

    #[kithara::test(native, flash(false))]
    fn a_header_carries_the_format_it_was_given() {
        let head = header(48_000, 1, None);

        assert_eq!(u16::from_le_bytes([head[22], head[23]]), 1);
        assert_eq!(field(&head, 24), 48_000);
        assert_eq!(field(&head, 28), 48_000 * 2);
    }

    #[kithara::test(native, flash(false))]
    fn a_sized_header_states_both_totals() {
        let data_bytes = 44_100 * 2 * 2;
        let head = header(44_100, 2, Some(data_bytes));

        assert_eq!(
            field(&head, 4),
            u32::try_from(36 + data_bytes).expect("fits")
        );
        assert_eq!(field(&head, 40), u32::try_from(data_bytes).expect("fits"));
    }

    #[kithara::test(native, flash(false))]
    fn a_streaming_header_states_neither_total() {
        let head = header(44_100, 2, None);

        assert_eq!(field(&head, 4), consts::UNKNOWN_SIZE);
        assert_eq!(field(&head, 40), consts::UNKNOWN_SIZE);
    }

    #[kithara::test(native, flash(false))]
    fn a_body_follows_its_header() {
        let bytes = wav(44_100, 2, 2, Wave::Sawtooth);

        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(i16::from_le_bytes([bytes[44], bytes[45]]), i16::MIN);
        assert_eq!(i16::from_le_bytes([bytes[48], bytes[49]]), i16::MIN + 1);
    }

    #[kithara::test(native, flash(false))]
    fn a_sized_wav_fills_the_byte_budget() {
        let bytes = wav_of_size(44_100, 2, 1_024, Wave::SawtoothDescending);

        assert_eq!(bytes.len(), 1_024);
        assert_eq!(i16::from_le_bytes([bytes[44], bytes[45]]), i16::MAX);
        assert_eq!(i16::from_le_bytes([bytes[48], bytes[49]]), i16::MAX - 1);
    }
}
