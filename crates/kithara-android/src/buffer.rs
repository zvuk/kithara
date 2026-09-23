use jni::{
    Env,
    objects::{Global, JByteBuffer},
};

use crate::error::AndroidBackendError;

/// A direct `java.nio.ByteBuffer` over a region owned here.
///
/// Hold it until the borrower reports it is done: a reference the borrower
/// kept outlives this value.
#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(crate) struct DirectBuffer<T> {
    /// The buffer to lend.
    #[field(get, vis = "pub(crate)")]
    object: Global<JByteBuffer<'static>>,
    region: Box<T>,
}

impl<T: AsMut<[u8]>> DirectBuffer<T> {
    /// Open a direct buffer over `region`.
    pub(crate) fn new(env: &mut Env<'_>, region: T) -> Result<Self, AndroidBackendError> {
        let mut region = Box::new(region);
        let bytes: &mut [u8] = (*region).as_mut();
        let len = bytes.len();
        let address = bytes.as_mut_ptr();

        // SAFETY: the region is boxed, so `address` names `len` bytes that
        // keep that address until this value drops. Java may keep its own
        // reference past that drop; soundness rests on the transport contract,
        // under which the transport touches a buffer only until the callback
        // answering it or ending its call, and on the owner holding this value
        // until that callback.
        let buffer = unsafe { env.new_direct_byte_buffer(address, len) }
            .map_err(AndroidBackendError::jni("jni-new-direct-byte-buffer"))?;
        let object = env
            .new_global_ref(&buffer)
            .map_err(AndroidBackendError::jni("jni-direct-byte-buffer-global"))?;
        Ok(Self { object, region })
    }

    /// Release Kithara's reference to the buffer and take the region back.
    pub(crate) fn into_inner(self) -> T {
        let Self { object, region } = self;
        drop(object);
        *region
    }
}
