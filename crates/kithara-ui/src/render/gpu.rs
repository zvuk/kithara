//! What the graphics device holds for this process.
//!
//! The renderer's own counters see only the resources it asked for; the bulk
//! of a frame's cost is the command-buffer storage the driver allocates
//! underneath them, which appears in no wgpu report. The device itself does
//! know, and answers in bytes — so a budget on the renderer is expressed
//! against [`allocated_bytes`], not against a resource count.
//!
//! Every backend that cannot answer returns `None`, which a caller reports as
//! "unmeasured" rather than as zero.

/// Bytes the process's graphics device currently holds, when the platform can
/// say.
///
/// `None` means this build has no device to ask — a non-Apple target today,
/// or a machine that offers no Metal device at all.
#[must_use]
pub fn allocated_bytes() -> Option<u64> {
    #[cfg(target_vendor = "apple")]
    {
        apple::allocated_bytes()
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        None
    }
}

#[cfg(target_vendor = "apple")]
mod apple {
    use objc2::{msg_send, rc::Retained, runtime::AnyObject};

    unsafe extern "C" {
        /// Returns the process's default Metal device, retained, or null on a
        /// machine that offers none.
        fn MTLCreateSystemDefaultDevice() -> *mut AnyObject;
    }

    pub(super) fn allocated_bytes() -> Option<u64> {
        // SAFETY: `MTLCreateSystemDefaultDevice` returns either null or a
        // device owned by the caller, which `Retained::from_raw` takes over
        // and releases on drop. `currentAllocatedSize` is a property of
        // `MTLDevice` returning `NSUInteger`, read here as `usize`.
        unsafe {
            let device = Retained::from_raw(MTLCreateSystemDefaultDevice())?;
            let bytes: usize = msg_send![&*device, currentAllocatedSize];
            Some(bytes as u64)
        }
    }
}
