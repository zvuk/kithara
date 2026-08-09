use std::{
    mem::{MaybeUninit, size_of, size_of_val, zeroed},
    os::raw::c_void,
    ptr,
};

use fdk_aac_sys as sys;

use crate::error::{EncodeError, EncodeResult};

struct Consts;
impl Consts {
    const CBR: u32 = 0;
    const ENCODER_MODULES: u32 = 0;
    const RAW_TRANSPORT: u32 = 0;
    const SBR_ON: u32 = 1;
}

pub(crate) struct Encoder {
    handle: sys::HANDLE_AACENCODER,
}

// SAFETY: the handle is this instance's alone and libfdk keeps no state shared
// between encoders, so moving one to another thread moves all of it. `Sync` is
// deliberately absent: two threads must not encode through one handle.
unsafe impl Send for Encoder {}

pub(crate) struct EncoderParams {
    pub(crate) aot: sys::AUDIO_OBJECT_TYPE,
    pub(crate) bit_rate: u32,
    pub(crate) channels: u16,
    pub(crate) sample_rate: u32,
    pub(crate) sbr: bool,
}

pub(crate) struct EncodeInfo {
    pub(crate) input_consumed: usize,
    pub(crate) output_size: usize,
}

impl Encoder {
    pub(crate) fn new(params: &EncoderParams) -> EncodeResult<Self> {
        let channel_mode = channel_mode(params.channels)?;
        let mut handle: sys::HANDLE_AACENCODER = ptr::null_mut();
        // SAFETY: aacEncOpen writes a valid handle on AACENC_OK.
        unsafe {
            check(sys::aacEncOpen(
                &mut handle as *mut _,
                Consts::ENCODER_MODULES,
                u32::from(params.channels),
            ))?;
        }
        let encoder = Self { handle };
        let aot = u32::try_from(params.aot).map_err(|_| {
            EncodeError::backend_message("audio object type does not fit into u32".to_owned())
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
                Consts::CBR,
            ))?;
            check(sys::aacEncoder_SetParam(
                handle,
                sys::AACENC_PARAM_AACENC_TRANSMUX,
                Consts::RAW_TRANSPORT,
            ))?;
            if params.sbr {
                check(sys::aacEncoder_SetParam(
                    handle,
                    sys::AACENC_PARAM_AACENC_SBR_MODE,
                    Consts::SBR_ON,
                ))?;
            }
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

    pub(crate) fn encode(&mut self, input: &[i16], output: &mut [u8]) -> EncodeResult<EncodeInfo> {
        let input_samples = i32::try_from(input.len()).map_err(|_| {
            EncodeError::backend_message("input sample count does not fit into i32".to_owned())
        })?;
        let (code, info) = self.run(input, output, input_samples)?;
        check(code)?;
        Ok(info)
    }

    pub(crate) fn flush(&mut self, output: &mut [u8]) -> EncodeResult<Option<EncodeInfo>> {
        const END_OF_INPUT: i32 = -1;

        let (code, info) = self.run(&[], output, END_OF_INPUT)?;
        if code == sys::AACENC_ERROR_AACENC_ENCODE_EOF {
            return Ok(None);
        }
        check(code)?;
        Ok(Some(info))
    }

    pub(crate) fn info(&self) -> EncodeResult<sys::AACENC_InfoStruct> {
        let mut info: MaybeUninit<sys::AACENC_InfoStruct> = MaybeUninit::uninit();
        // SAFETY: aacEncInfo initializes the struct before returning AACENC_OK.
        unsafe {
            check(sys::aacEncInfo(self.handle, info.as_mut_ptr()))?;
            Ok(info.assume_init())
        }
    }

    fn run(
        &mut self,
        input: &[i16],
        output: &mut [u8],
        input_samples: i32,
    ) -> EncodeResult<(sys::AACENC_ERROR, EncodeInfo)> {
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

        let in_args = sys::AACENC_InArgs {
            numInSamples: input_samples,
            numAncBytes: 0,
        };
        // SAFETY: AACENC_OutArgs is a C POD whose zero value is valid input.
        let mut out_args: sys::AACENC_OutArgs = unsafe { zeroed() };

        // SAFETY: descriptors reference valid slices for the duration of this call.
        let code = unsafe {
            sys::aacEncEncode(
                self.handle,
                &input_desc,
                &output_desc,
                &in_args,
                &mut out_args,
            )
        };

        Ok((
            code,
            EncodeInfo {
                input_consumed: usize::try_from(out_args.numInSamples).unwrap_or(0),
                output_size: usize::try_from(out_args.numOutBytes).unwrap_or(0),
            },
        ))
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

pub(crate) fn audio_specific_config(info: &sys::AACENC_InfoStruct) -> Vec<u8> {
    let len = info.confSize as usize;
    info.confBuf[..len].to_vec()
}

fn channel_mode(channels: u16) -> EncodeResult<u32> {
    let mode = match channels {
        1 => sys::CHANNEL_MODE_MODE_1,
        2 => sys::CHANNEL_MODE_MODE_2,
        3 => sys::CHANNEL_MODE_MODE_1_2,
        4 => sys::CHANNEL_MODE_MODE_1_2_1,
        5 => sys::CHANNEL_MODE_MODE_1_2_2,
        6 => sys::CHANNEL_MODE_MODE_1_2_2_1,
        channels => {
            return Err(EncodeError::InvalidInput(format!(
                "fdk-aac carries no channel mode for {channels} channels"
            )));
        }
    };
    u32::try_from(mode)
        .map_err(|_| EncodeError::backend_message("channel mode does not fit into u32".to_owned()))
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
