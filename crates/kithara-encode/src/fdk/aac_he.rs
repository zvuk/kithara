use std::{
    cmp,
    mem::{MaybeUninit, size_of, size_of_val, zeroed},
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
    const ACCESS_UNIT_CAPACITY: usize = 8 * 1024;
    const CHANNELS: u16 = 2;
    const ENCODER_MODULES: u32 = 0;
    const FRAME_BUFFER_COUNT: usize = 2;
    const FRAME_OUTPUT_SAMPLES: usize = 2048;
}

#[derive(Clone, Copy)]
pub(crate) enum AacHeProfile {
    V1,
    V2,
}

impl AacHeProfile {
    const fn aot(self) -> sys::AUDIO_OBJECT_TYPE {
        match self {
            Self::V1 => sys::AUDIO_OBJECT_TYPE_AOT_SBR,
            Self::V2 => sys::AUDIO_OBJECT_TYPE_AOT_PS,
        }
    }

    const fn codec(self) -> AudioCodec {
        match self {
            Self::V1 => AudioCodec::AacHe,
            Self::V2 => AudioCodec::AacHeV2,
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::V1 => "HE-AAC v1",
            Self::V2 => "HE-AAC v2",
        }
    }
}

pub(crate) struct AacHeEncoder;

impl AacHeEncoder {
    pub(crate) fn encode(
        request: &PackagedEncodeRequest<'_>,
        profile: AacHeProfile,
    ) -> EncodeResult<EncodedTrack> {
        request.validate()?;

        let sample_rate = request.pcm.sample_rate();
        let channels = request.pcm.channels();
        if channels != Consts::CHANNELS {
            return Err(EncodeError::InvalidInput(format!(
                "{} requires stereo input (channels={})",
                profile.name(),
                Consts::CHANNELS
            )));
        }

        let encoder = Encoder::new(&EncoderParams {
            profile,
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
        let frame_samples = u64::try_from(frame_input_samples).map_err(|_| {
            EncodeError::backend_message("frame sample count does not fit into u64".to_owned())
        })? / u64::from(channels);
        let frame_pts_step = frame_samples * u64::from(timescale) / u64::from(sample_rate);
        let frame_pts_step = frame_pts_step.max(1);
        let frame_duration = u32::try_from(frame_pts_step).map_err(|_| {
            EncodeError::backend_message("frame duration does not fit into u32".to_owned())
        })?;

        pump_pcm_into_encoder(request.pcm, frame_input_samples, |input| {
            let mut output = [0u8; Consts::ACCESS_UNIT_CAPACITY];
            let encoded = encoder.encode(input, &mut output)?;
            if encoded.output_size > 0 {
                units.push(EncodedAccessUnit {
                    pts,
                    bytes: output[..encoded.output_size].to_vec(),
                    dts: pts,
                    duration: frame_duration,
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
                pts,
                bytes: output[..encoded.output_size].to_vec(),
                dts: pts,
                duration: frame_duration,
                is_sync: true,
            });
            pts = pts.saturating_add(frame_pts_step);
        }

        let mut media_info = request.media_info.clone();
        media_info.codec = Some(profile.codec());
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
    let mut buf: Vec<i16> = Vec::with_capacity(frame_input_samples * Consts::FRAME_BUFFER_COUNT);
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
    profile: AacHeProfile,
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
            check(sys::aacEncOpen(
                &mut handle as *mut _,
                Consts::ENCODER_MODULES,
                u32::from(Consts::CHANNELS),
            ))?;
        }
        let encoder = Self { handle };
        let aot = u32::try_from(params.profile.aot()).map_err(|_| {
            EncodeError::backend_message("audio object type does not fit into u32".to_owned())
        })?;
        let channel_mode = u32::try_from(sys::CHANNEL_MODE_MODE_2).map_err(|_| {
            EncodeError::backend_message("channel mode does not fit into u32".to_owned())
        })?;

        // SAFETY: handle is non-null after aacEncOpen succeeded.
        unsafe {
            check(sys::aacEncoder_SetParam(
                handle,
                sys::AACENC_PARAM_AACENC_AOT,
                aot,
            ))?;
            check(sys::aacEncoder_SetParam(
                handle,
                sys::AACENC_PARAM_AACENC_SAMPLERATE,
                params.sample_rate,
            ))?;
            check(sys::aacEncoder_SetParam(
                handle,
                sys::AACENC_PARAM_AACENC_CHANNELMODE,
                channel_mode,
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
        let mut input_buf_el_size: i32 = i32::try_from(size_of::<i16>()).map_err(|_| {
            EncodeError::backend_message("input sample size does not fit into i32".to_owned())
        })?;
        let input_desc = sys::AACENC_BufDesc {
            numBufs: 1,
            bufs: ptr::addr_of_mut!(input_buf_ptr).cast::<*mut c_void>(),
            bufferIdentifiers: &mut input_buf_ident,
            bufSizes: &mut input_buf_size,
            bufElSizes: &mut input_buf_el_size,
        };

        let mut output_buf_ptr = output.as_mut_ptr();
        let mut output_buf_ident: i32 = sys::AACENC_BufferIdentifier_OUT_BITSTREAM_DATA as i32;
        let mut output_buf_size: i32 = i32::try_from(output.len())
            .map_err(|_| EncodeError::backend_message("output slice too large".to_owned()))?;
        let mut output_buf_el_size: i32 = i32::try_from(size_of::<u8>()).map_err(|_| {
            EncodeError::backend_message("output element size does not fit into i32".to_owned())
        })?;
        let output_desc = sys::AACENC_BufDesc {
            numBufs: 1,
            bufs: ptr::addr_of_mut!(output_buf_ptr).cast::<*mut c_void>(),
            bufferIdentifiers: &mut output_buf_ident,
            bufSizes: &mut output_buf_size,
            bufElSizes: &mut output_buf_el_size,
        };

        let input_samples = i32::try_from(input_len).map_err(|_| {
            EncodeError::backend_message("input sample count does not fit into i32".to_owned())
        })?;
        let in_args = sys::AACENC_InArgs {
            numInSamples: input_samples,
            numAncBytes: 0,
        };
        // SAFETY: AACENC_OutArgs is a C POD whose zero value is valid input.
        let mut out_args: sys::AACENC_OutArgs = unsafe { zeroed() };

        // SAFETY: descriptors reference valid slices for the duration of this call.
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
        // SAFETY: aacEncInfo initializes the struct before returning AACENC_OK.
        unsafe {
            check(sys::aacEncInfo(self.handle, info.as_mut_ptr()))?;
            Ok(info.assume_init())
        }
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            // SAFETY: this instance owns the handle returned by aacEncOpen.
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
