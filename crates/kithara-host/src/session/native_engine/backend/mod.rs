mod callback;
mod config;
mod device;
mod stream;

pub(super) use config::cpal_config;
pub(super) use device::AcquiredOutput;
pub(crate) use stream::NativeStream;
