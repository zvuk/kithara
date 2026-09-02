use std::{cell::Cell, marker::PhantomData, ptr::NonNull, slice};

use bungee_sys::{BungeeStretcher, InputChunk, OutputChunk, Request, SampleRates, stretcher};
use kithara_test_macros as kithara;

use crate::{BungeeConfig, ElasticError};

#[derive(Clone, Copy)]
pub(super) struct AnalysisInput<'a> {
    pub(super) samples: &'a [f32],
    pub(super) channel_stride: usize,
    pub(super) mute_head: usize,
    pub(super) mute_tail: usize,
}

pub(super) struct NativeOutput {
    pub(super) valid: bool,
    pub(super) begin: f64,
    pub(super) end: f64,
    pub(super) frames: usize,
}

pub(super) struct NativeStretcher {
    inner: NonNull<BungeeStretcher>,
    channels: usize,
    #[cfg(test)]
    fault: Option<NativeFault>,
    not_sync: PhantomData<Cell<()>>,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum NativeFault {
    Analyse,
    Synthesise,
}

// SAFETY: the native handle has unique ownership and every operation requires
// `&mut self`; moving that owner between threads cannot create concurrent use.
unsafe impl Send for NativeStretcher {}

impl NativeStretcher {
    const OUTPUT_ENDPOINT_COUNT: usize = 2;

    pub(super) fn new(
        sample_rate: u32,
        channels: usize,
        config: BungeeConfig,
    ) -> Result<Self, ElasticError> {
        let input = i32::try_from(sample_rate)
            .map_err(|_| ElasticError::EnginePreparation("Bungee sample rate is out of range"))?;
        let channels = i32::try_from(channels)
            .map_err(|_| ElasticError::EnginePreparation("Bungee channel count is out of range"))?;
        let inner = stretcher::create(
            SampleRates {
                input,
                output: input,
            },
            channels,
            config.log2_synthesis_hop_adjust(),
        );
        Ok(Self {
            inner: NonNull::new(inner).ok_or(ElasticError::EnginePreparation(
                "Bungee stretcher creation failed",
            ))?,
            channels: usize::try_from(channels).map_err(|_| {
                ElasticError::EnginePreparation("Bungee channel count is out of range")
            })?,
            #[cfg(test)]
            fault: None,
            not_sync: PhantomData,
        })
    }

    #[kithara::measure]
    pub(super) fn analyse(&mut self, input: AnalysisInput<'_>) -> Result<(), ElasticError> {
        #[cfg(test)]
        if self.take_fault(NativeFault::Analyse) {
            return Err(ElasticError::EnginePreparation(
                "injected Bungee analysis failure",
            ));
        }
        let required = planar_len(self.channels, input.channel_stride)?;
        if input.samples.len() < required {
            return Err(ElasticError::EnginePreparation(
                "Bungee input grain storage is too small",
            ));
        }
        let channel_stride = isize::try_from(input.channel_stride).map_err(|_| {
            ElasticError::EnginePreparation("Bungee input channel stride is out of range")
        })?;
        let mute_head = i32::try_from(input.mute_head).map_err(|_| {
            ElasticError::EnginePreparation("Bungee muted input head is out of range")
        })?;
        let mute_tail = i32::try_from(input.mute_tail).map_err(|_| {
            ElasticError::EnginePreparation("Bungee muted input tail is out of range")
        })?;
        stretcher::analyse_grain(
            self.inner.as_ptr(),
            input.samples.as_ptr(),
            channel_stride,
            mute_head,
            mute_tail,
        );
        Ok(())
    }

    #[kithara::measure]
    fn copy_native_output(
        &self,
        output: &OutputChunk,
        destination: &mut [f32],
        destination_stride: usize,
    ) -> Result<NativeOutput, ElasticError> {
        let frames = usize::try_from(output.frame_count).map_err(|_| {
            ElasticError::EnginePreparation("Bungee reported a negative output frame count")
        })?;
        let source_stride = usize::try_from(output.channel_stride).map_err(|_| {
            ElasticError::EnginePreparation("Bungee reported a negative output channel stride")
        })?;
        if frames > destination_stride || (frames > 0 && source_stride < frames) {
            return Err(ElasticError::EnginePreparation(
                "Bungee reported an invalid output channel stride",
            ));
        }
        let required = planar_extent(self.channels, destination_stride, frames)?;
        if destination.len() < required {
            return Err(ElasticError::EnginePreparation(
                "Bungee output grain storage is too small",
            ));
        }
        if frames > 0 {
            let data = NonNull::new(output.data).ok_or(ElasticError::EnginePreparation(
                "Bungee returned null output audio",
            ))?;
            planar_extent(self.channels, source_stride, frames)?;
            // SAFETY: Bungee owns `data` through this call and documents one
            // `frames`-long channel at every checked `source_stride` offset.
            // The helper copies immediately; no native pointer is retained.
            unsafe {
                copy_planar(
                    data,
                    source_stride,
                    frames,
                    self.channels,
                    destination,
                    destination_stride,
                );
            }
        }

        let begin = request_position(output.request[0])?;
        let end = request_position(output.request[1])?;
        Ok(NativeOutput {
            begin,
            end,
            frames,
            valid: begin.is_finite(),
        })
    }

    #[cfg(test)]
    pub(super) fn fail_next(&mut self, fault: NativeFault) {
        self.fault = Some(fault);
    }

    pub(super) fn is_flushed(&self) -> bool {
        stretcher::is_flushed(self.inner.as_ptr()) != 0
    }

    pub(super) fn max_input_frames(&self) -> Result<usize, ElasticError> {
        usize::try_from(stretcher::max_input_frame_count(self.inner.as_ptr()))
            .ok()
            .filter(|frames| *frames > 0)
            .ok_or(ElasticError::EnginePreparation(
                "Bungee reported an invalid input grain size",
            ))
    }

    pub(super) fn next(&mut self, request: &mut Request) {
        stretcher::next(self.inner.as_ptr(), request);
    }

    pub(super) fn preroll(&mut self, request: &mut Request) {
        stretcher::preroll(self.inner.as_ptr(), request);
    }

    pub(super) fn specify(&mut self, request: &Request) -> InputChunk {
        stretcher::specify_grain(self.inner.as_ptr(), request, 0.0)
    }

    pub(super) fn synthesise(
        &mut self,
        destination: &mut [f32],
        destination_stride: usize,
    ) -> Result<NativeOutput, ElasticError> {
        let output = self.synthesise_native();
        #[cfg(test)]
        if self.take_fault(NativeFault::Synthesise) {
            return Err(ElasticError::EnginePreparation(
                "injected Bungee synthesis failure",
            ));
        }
        self.copy_native_output(&output, destination, destination_stride)
    }

    #[kithara::measure]
    fn synthesise_native(&mut self) -> OutputChunk {
        let mut output = OutputChunk {
            data: std::ptr::null_mut(),
            frame_count: i32::default(),
            channel_stride: isize::default(),
            request: [std::ptr::null(); Self::OUTPUT_ENDPOINT_COUNT],
        };
        stretcher::synthesise_grain(self.inner.as_ptr(), &mut output);
        output
    }

    #[cfg(test)]
    fn take_fault(&mut self, fault: NativeFault) -> bool {
        if self.fault == Some(fault) {
            self.fault = None;
            true
        } else {
            false
        }
    }
}

impl Drop for NativeStretcher {
    fn drop(&mut self) {
        stretcher::destroy(self.inner.as_ptr());
    }
}

fn planar_len(channels: usize, stride: usize) -> Result<usize, ElasticError> {
    channels
        .checked_mul(stride)
        .ok_or(ElasticError::SampleCountOverflow)
}

fn planar_extent(channels: usize, stride: usize, frames: usize) -> Result<usize, ElasticError> {
    channels
        .checked_sub(1)
        .and_then(|last| last.checked_mul(stride))
        .and_then(|offset| offset.checked_add(frames))
        .ok_or(ElasticError::SampleCountOverflow)
}

fn request_position(request: *const Request) -> Result<f64, ElasticError> {
    let request = NonNull::new(request.cast_mut()).ok_or(ElasticError::EnginePreparation(
        "Bungee returned null output timing",
    ))?;
    // SAFETY: Bungee initializes `position` for every grain. Reading only that field avoids referencing the partially initialized
    // `Request`, and the pointer remains valid until the next native call.
    Ok(unsafe { std::ptr::addr_of!((*request.as_ptr()).position).read() })
}

unsafe fn copy_planar(
    source: NonNull<f32>,
    source_stride: usize,
    frames: usize,
    channels: usize,
    destination: &mut [f32],
    destination_stride: usize,
) {
    for channel in 0..channels {
        let source_offset = channel * source_stride;
        let destination_offset = channel * destination_stride;
        // SAFETY: the caller checked every offset and extent against the
        // native chunk contract and destination slice before entering.
        let source = unsafe { slice::from_raw_parts(source.as_ptr().add(source_offset), frames) };
        destination[destination_offset..destination_offset + frames].copy_from_slice(source);
    }
}

#[cfg(test)]
mod tests {
    use std::mem::MaybeUninit;

    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn planar_copy_respects_native_channel_stride() {
        let mut native = [1.0, 2.0, f32::NAN, 3.0, 4.0, f32::NAN];
        let mut copied = [0.0; 4];
        let source = NonNull::new(native.as_mut_ptr()).expect("array pointer is non-null");

        // SAFETY: the source has two channels of two frames at stride three;
        // both source and destination extents are statically in bounds.
        unsafe { copy_planar(source, 3, 2, 2, &mut copied, 2) };

        assert_eq!(copied, [1.0, 2.0, 3.0, 4.0]);
    }

    #[kithara::test]
    fn timing_read_does_not_require_a_fully_initialized_request() {
        let mut request = MaybeUninit::<Request>::uninit();
        let request_ptr = request.as_mut_ptr();
        // SAFETY: this fixture intentionally mirrors Bungee's initial grains,
        // where `position` is initialized while later fields may not be.
        unsafe { std::ptr::addr_of_mut!((*request_ptr).position).write(42.0) };

        let position = request_position(request_ptr).expect("initialized position is readable");

        assert_eq!(position, 42.0);
    }
}
