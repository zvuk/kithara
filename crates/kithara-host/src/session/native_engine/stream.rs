use firewheel::{
    ActivateInfo,
    cpal::cpal::{
        self, Device, FromSample, SampleFormat, SizedSample, StreamConfig,
        traits::{DeviceTrait, StreamTrait},
    },
};
use ringbuf::{
    HeapProd, HeapRb,
    traits::{Observer, Producer, Split},
};

use super::{callback::OutputCallback, device::AcquiredOutput};
use crate::session::processor::SessionProcessor;

/// Owns the device callback, whose processor settles before returning to its context.
pub(crate) struct NativeStream {
    stream: cpal::Stream,
    install: HeapProd<SessionProcessor>,
    pub(in crate::session::native_engine) info: ActivateInfo,
}

impl AcquiredOutput {
    pub(in crate::session::native_engine) fn open(self) -> Result<NativeStream, String> {
        let channels = usize::from(self.config.channels);
        let (install, incoming) = HeapRb::new(1).split();
        let mut callback = OutputCallback::new(incoming, channels, self.info.sample_rate);
        let capacity = self.conversion_samples;
        let stream = match self.format {
            SampleFormat::F32 => self.device.build_output_stream(
                self.config,
                move |data: &mut [f32], info| callback.process(data, info),
                |error| report_error(&error),
                None,
            ),
            SampleFormat::I8 => converted::<i8>(&self.device, self.config, callback, capacity),
            SampleFormat::I16 => converted::<i16>(&self.device, self.config, callback, capacity),
            SampleFormat::I32 => converted::<i32>(&self.device, self.config, callback, capacity),
            SampleFormat::I64 => converted::<i64>(&self.device, self.config, callback, capacity),
            SampleFormat::U8 => converted::<u8>(&self.device, self.config, callback, capacity),
            SampleFormat::U16 => converted::<u16>(&self.device, self.config, callback, capacity),
            SampleFormat::U32 => converted::<u32>(&self.device, self.config, callback, capacity),
            SampleFormat::U64 => converted::<u64>(&self.device, self.config, callback, capacity),
            SampleFormat::F64 => converted::<f64>(&self.device, self.config, callback, capacity),
            format => return Err(format!("unsupported output sample format {format}")),
        }
        .map_err(|error| error.to_string())?;
        Ok(NativeStream {
            stream,
            install,
            info: self.info,
        })
    }
}

impl NativeStream {
    pub(in crate::session::native_engine) fn reserve_processor(&self) -> Result<(), String> {
        if self.install.is_full() {
            return Err("native processor installation is still occupied".into());
        }
        Ok(())
    }

    pub(in crate::session::native_engine) fn start(
        mut self,
        processor: SessionProcessor,
    ) -> Result<Self, String> {
        if let Err(processor) = self.install.try_push(processor) {
            drop(processor);
            return Err("native processor installation is still occupied".into());
        }
        self.stream.play().map_err(|error| error.to_string())?;
        Ok(self)
    }
}

fn converted<T: SizedSample + FromSample<f32>>(
    device: &Device,
    config: StreamConfig,
    mut callback: OutputCallback,
    capacity: usize,
) -> Result<cpal::Stream, cpal::Error> {
    let mut scratch = [0.0_f32; crate::consts::SESSION_OUTPUT_CONVERSION_SAMPLES];
    device.build_output_stream(
        config,
        move |output: &mut [T], info| {
            for output in output.chunks_mut(capacity) {
                let scratch = scratch
                    .get_mut(..output.len())
                    .expect("conversion chunk fits prepared scratch");
                callback.process(scratch, info);
                for (output, sample) in output.iter_mut().zip(scratch) {
                    *output = T::from_sample_(*sample);
                }
            }
        },
        |error| report_error(&error),
        None,
    )
}

fn report_error(error: &cpal::Error) {
    tracing::error!(%error, "native output stream failed");
}
