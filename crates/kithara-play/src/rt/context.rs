use firewheel::node::{ProcInfo, ProcStore};
use kithara_warp::RenderContext;

#[derive(Debug, Default)]
enum RenderContextSlot {
    #[default]
    Unwritten,
    Ready(RenderContext),
    Invalid,
}

/// Installs the preallocated slot shared by the Host transport writer and its
/// session-created Player nodes.
///
/// # Errors
///
/// Returns an error when the slot has already been installed.
#[doc(hidden)]
pub fn install_render_context(store: &mut ProcStore) -> Result<(), &'static str> {
    store
        .insert(RenderContextSlot::default())
        .map_err(|_| "render context store slot already exists")
}

/// Replaces the shared session render context for the current graph block.
///
/// # Errors
///
/// Returns an error when the Host did not install the slot before stream start.
#[doc(hidden)]
pub fn publish_render_context(
    store: &mut ProcStore,
    context: RenderContext,
) -> Result<(), &'static str> {
    replace(store, RenderContextSlot::Ready(context))
}

/// Invalidates the shared context so a later Player node cannot reuse an older
/// graph block.
///
/// # Errors
///
/// Returns an error when the Host did not install the slot before stream start.
#[doc(hidden)]
pub fn invalidate_render_context(store: &mut ProcStore) -> Result<(), &'static str> {
    replace(store, RenderContextSlot::Invalid)
}

fn replace(store: &mut ProcStore, next: RenderContextSlot) -> Result<(), &'static str> {
    let slot = store
        .try_get_mut::<RenderContextSlot>()
        .ok_or("render context store slot is missing")?;
    *slot = next;
    Ok(())
}

/// Reads the context written for this exact process block.
///
/// # Errors
///
/// Returns an allocation-free diagnostic when the slot is absent, invalid, or
/// does not match `info` exactly.
#[doc(hidden)]
pub fn read_render_context<'a>(
    store: &'a ProcStore,
    info: &ProcInfo,
) -> Result<&'a RenderContext, &'static str> {
    let slot = store
        .try_get::<RenderContextSlot>()
        .ok_or("render context store slot is missing")?;
    let context = match slot {
        RenderContextSlot::Unwritten => {
            return Err("render context was not written before player processing");
        }
        RenderContextSlot::Ready(context) => context,
        RenderContextSlot::Invalid => return Err("render context is invalid"),
    };
    let frames = i64::try_from(info.frames)
        .map_err(|_| "render context does not match the player process block")?;
    let end = info
        .clock_samples
        .0
        .checked_add(frames)
        .ok_or("render context does not match the player process block")?;
    let output_frames = context.output_frames();
    if context.sample_rate() != info.sample_rate
        || i64::from(output_frames.start) != info.clock_samples.0
        || i64::from(output_frames.end) != end
    {
        return Err("render context does not match the player process block");
    }
    Ok(context)
}
