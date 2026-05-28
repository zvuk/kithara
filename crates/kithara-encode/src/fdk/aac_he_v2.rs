use std::{
    cmp,
    mem::{MaybeUninit, size_of, size_of_val},
    os::raw::c_void,
    ptr,
};

use fdk_aac_sys as sys;
use kithara_stream::{AudioCodec, ContainerFormat};

use crate::{
    EncodeError, EncodeResult,
    types::{EncodedAccessUnit, EncodedTrack, PackagedEncodeRequest, PcmSource},
};

struct Consts;
impl Consts {
    /// Soft upper bound on the encoded byte size of a single
    /// access unit. HE-AAC v2 frames at 32 kbps @ `44_100` Hz fit
    /// well under `1 KiB`; `8 KiB` leaves head-room for higher
    /// bitrates.
    const ACCESS_UNIT_CAPACITY: usize = 8 * 1024;

    /// Each HE-AAC v2 access unit holds 1024 base-AAC samples
    /// upsampled by SBR to 2048 output samples at the encoder's
    /// declared (output) sample rate. The encoder consumes 2048
    /// input samples per output frame.
    const FRAME_OUTPUT_SAMPLES: usize = 2048;
}

pub(crate) struct AacHeV2Encoder;

impl AacHeV2Encoder {
    pub(crate) fn encode(request: &PackagedEncodeRequest<'_>) -> EncodeResult<EncodedTrack> {
        validate(request)?;

        let sample_rate = request.pcm.sample_rate();
        let channels = request.pcm.channels();
        if channels != 2 {
            return Err(EncodeError::InvalidInput(
                "HE-AAC v2 requires stereo input (channels=2)".to_owned(),
            ));
        }

        // SAFETY: handle is owned by `Encoder` for the lifetime of
        let encoder = Encoder::new(&EncoderParams {
            sample_rate,
            bit_rate: request.bit_rate.try_into().map_err(|_| {
                EncodeError::InvalidInput("bit_rate does not fit into u32".to_owned())
            })?,
        })?;
        let info = encoder.info()?;
        let default_frame_input = Consts::FRAME_OUTPUT_SAMPLES * usize::from(channels);
        let frame_input_samples = usize::try_from(info.frameLength)
            .ok()
            .map(|n| n * usize::from(channels))
            .filter(|n| *n > 0)
            .unwrap_or(default_frame_input);

        let asc = Encoder::audio_specific_config(&info);

        let mut units: Vec<EncodedAccessUnit> = Vec::new();
        let mut pts: u64 = 0;
        let timescale = request.timescale;
        let frame_pts_step = (u64::try_from(frame_input_samples).unwrap_or(u64::MAX)
            / u64::from(channels))
            * u64::from(timescale)
            / u64::from(sample_rate);
        let frame_pts_step = frame_pts_step.max(1);

        pump_pcm_into_encoder(request.pcm, frame_input_samples, |input| {
            let mut output = [0u8; Consts::ACCESS_UNIT_CAPACITY];
            let encoded = encoder.encode(input, &mut output)?;
            if encoded.output_size > 0 {
                units.push(EncodedAccessUnit {
                    bytes: output[..encoded.output_size].to_vec(),
                    pts,
                    dts: pts,
                    duration: u32::try_from(frame_pts_step).unwrap_or(u32::MAX),
                    is_sync: true,
                });
                pts = pts.saturating_add(frame_pts_step);
            }
            Ok::<usize, EncodeError>(encoded.input_consumed)
        })?;

        let empty: [i16; 0] = [];
        loop {
            let mut output = [0u8; Consts::ACCESS_UNIT_CAPACITY];
            let encoded = encoder.encode(&empty, &mut output)?;
            if encoded.output_size == 0 {
                break;
            }
            units.push(EncodedAccessUnit {
                bytes: output[..encoded.output_size].to_vec(),
                pts,
                dts: pts,
                duration: u32::try_from(frame_pts_step).unwrap_or(u32::MAX),
                is_sync: true,
            });
            pts = pts.saturating_add(frame_pts_step);
        }

        let mut media_info = request.media_info.clone();
        media_info.codec = Some(AudioCodec::AacHeV2);
        media_info.container = Some(ContainerFormat::Fmp4);
        media_info.sample_rate = Some(sample_rate);
        media_info.channels = Some(channels);

        Ok(EncodedTrack {
            media_info,
            timescale: request.timescale,
            bit_rate: request.bit_rate,
            codec_config: asc,
            packets_per_segment: request.packets_per_segment,
            encoder_delay: request.encoder_delay,
            trailing_delay: request.trailing_delay,
            access_units: units,
        })
    }

    pub(crate) const fn frame_samples() -> usize {
        Consts::FRAME_OUTPUT_SAMPLES
    }
}

fn validate(request: &PackagedEncodeRequest<'_>) -> EncodeResult<()> {
    if request.timescale == 0 {
        return Err(EncodeError::InvalidInput(
            "timescale must be > 0".to_owned(),
        ));
    }
    if request.packets_per_segment == 0 {
        return Err(EncodeError::InvalidInput(
            "packets_per_segment must be > 0".to_owned(),
        ));
    }
    if request.pcm.total_byte_len().is_none() {
        return Err(EncodeError::InvalidInput(
            "PCM source must have a finite length".to_owned(),
        ));
    }
    Ok(())
}

/// Streams interleaved i16 samples from `pcm` into `feed` in
/// chunks of `frame_input_samples` (channels × per-channel samples
/// per frame). `feed` returns how many samples (i16 elements) the
/// encoder actually consumed — we slide forward by that amount.
fn pump_pcm_into_encoder<F>(
    pcm: &dyn PcmSource,
    frame_input_samples: usize,
    mut feed: F,
) -> EncodeResult<()>
where
    F: FnMut(&[i16]) -> EncodeResult<usize>,
{
    let total = pcm.total_byte_len().unwrap_or(0);
    let bytes_per_sample = size_of::<i16>();
    let frame_bytes = frame_input_samples * bytes_per_sample;
    let mut byte_offset: usize = 0;
    let mut buf: Vec<i16> = Vec::with_capacity(frame_input_samples * 2);
    let mut raw: Vec<u8> = vec![0u8; frame_bytes];
    while byte_offset < total {
        let want = cmp::min(frame_bytes, total - byte_offset);
        let n = pcm.read_pcm_at(byte_offset, &mut raw[..want]);
        if n == 0 {
            break;
        }
        byte_offset += n;
        buf.extend(
            raw[..n]
                .chunks_exact(bytes_per_sample)
                .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]])),
        );
        while buf.len() >= frame_input_samples {
            let consumed = feed(&buf[..frame_input_samples])?;
            if consumed == 0 {
                break;
            }
            buf.drain(..consumed);
        }
    }
    Ok(())
}

struct Encoder {
    handle: sys::HANDLE_AACENCODER,
}

struct EncoderParams {
    bit_rate: u32,
    sample_rate: u32,
}

struct EncodeInfo {
    input_consumed: usize,
    output_size: usize,
}

impl Encoder {
    fn new(params: &EncoderParams) -> EncodeResult<Self> {
        let mut handle: sys::HANDLE_AACENCODER = ptr::null_mut();
        // SAFETY: aacEncOpen writes a valid handle on AACENC_OK.
        unsafe {
            check(sys::aacEncOpen(&mut handle as *mut _, 0, 2))?;
        }
        let encoder = Self { handle };

        // SAFETY: handle is non-null after aacEncOpen succeeded.
        unsafe {
            check(sys::aacEncoder_SetParam(
                handle,
                sys::AACENC_PARAM_AACENC_AOT,
                sys::AUDIO_OBJECT_TYPE_AOT_PS as u32,
            ))?;
            check(sys::aacEncoder_SetParam(
                handle,
                sys::AACENC_PARAM_AACENC_SAMPLERATE,
                params.sample_rate,
            ))?;
            check(sys::aacEncoder_SetParam(
                handle,
                sys::AACENC_PARAM_AACENC_CHANNELMODE,
                2,
            ))?;
            check(sys::aacEncoder_SetParam(
                handle,
                sys::AACENC_PARAM_AACENC_BITRATE,
                params.bit_rate,
            ))?;
            check(sys::aacEncoder_SetParam(
                handle,
                sys::AACENC_PARAM_AACENC_BITRATEMODE,
                0,
            ))?;
            check(sys::aacEncoder_SetParam(
                handle,
                sys::AACENC_PARAM_AACENC_TRANSMUX,
                0,
            ))?;
            check(sys::aacEncoder_SetParam(
                handle,
                sys::AACENC_PARAM_AACENC_SBR_MODE,
                1,
            ))?;
            check(sys::aacEncEncode(
                handle,
                ptr::null(),
                ptr::null(),
                ptr::null(),
                ptr::null_mut(),
            ))?;
        }

        Ok(encoder)
    }

    fn audio_specific_config(info: &sys::AACENC_InfoStruct) -> Vec<u8> {
        let len = info.confSize as usize;
        info.confBuf[..len].to_vec()
    }

    fn encode(&self, input: &[i16], output: &mut [u8]) -> EncodeResult<EncodeInfo> {
        let input_len = input.len();
        let mut input_buf_ptr = input.as_ptr().cast_mut();
        let mut input_buf_ident: i32 = sys::AACENC_BufferIdentifier_IN_AUDIO_DATA as i32;
        let mut input_buf_size: i32 = i32::try_from(size_of_val(input))
            .map_err(|_| EncodeError::backend_message("input slice too large".to_owned()))?;
        let mut input_buf_el_size: i32 = i32::try_from(size_of::<i16>()).unwrap_or(i32::MAX);
        let input_desc = sys::AACENC_BufDesc {
            numBufs: 1,
            bufs: std::ptr::addr_of_mut!(input_buf_ptr).cast::<*mut c_void>(),
            bufferIdentifiers: &mut input_buf_ident,
            bufSizes: &mut input_buf_size,
            bufElSizes: &mut input_buf_el_size,
        };

        let mut output_buf_ptr = output.as_mut_ptr();
        let mut output_buf_ident: i32 = sys::AACENC_BufferIdentifier_OUT_BITSTREAM_DATA as i32;
        let mut output_buf_size: i32 = i32::try_from(output.len()).unwrap_or(i32::MAX);
        let mut output_buf_el_size: i32 = i32::try_from(size_of::<u8>()).unwrap_or(i32::MAX);
        let output_desc = sys::AACENC_BufDesc {
            numBufs: 1,
            bufs: std::ptr::addr_of_mut!(output_buf_ptr).cast::<*mut c_void>(),
            bufferIdentifiers: &mut output_buf_ident,
            bufSizes: &mut output_buf_size,
            bufElSizes: &mut output_buf_el_size,
        };

        let in_args = sys::AACENC_InArgs {
            numInSamples: i32::try_from(input_len).unwrap_or(i32::MAX),
            numAncBytes: 0,
        };
        // SAFETY: `AACENC_OutArgs` is a plain POD struct of two
        let mut out_args: sys::AACENC_OutArgs = unsafe { core::mem::zeroed() };

        // SAFETY: all buffers + descriptors above point into valid
        unsafe {
            check(sys::aacEncEncode(
                self.handle,
                &input_desc,
                &output_desc,
                &in_args,
                &mut out_args,
            ))?;
        }

        Ok(EncodeInfo {
            input_consumed: usize::try_from(out_args.numInSamples).unwrap_or(0),
            output_size: usize::try_from(out_args.numOutBytes).unwrap_or(0),
        })
    }

    fn info(&self) -> EncodeResult<sys::AACENC_InfoStruct> {
        let mut info: MaybeUninit<sys::AACENC_InfoStruct> = MaybeUninit::uninit();
        // SAFETY: aacEncInfo writes a fully-initialised struct on
        unsafe {
            check(sys::aacEncInfo(self.handle, info.as_mut_ptr()))?;
            Ok(info.assume_init())
        }
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            // SAFETY: handle was returned by aacEncOpen and not
            unsafe {
                sys::aacEncClose(&mut self.handle as *mut _);
            }
        }
    }
}

fn check(code: sys::AACENC_ERROR) -> EncodeResult<()> {
    if code == sys::AACENC_ERROR_AACENC_OK {
        Ok(())
    } else {
        Err(EncodeError::backend_message(format!(
            "fdk-aac encoder error: {code:?}"
        )))
    }
}
