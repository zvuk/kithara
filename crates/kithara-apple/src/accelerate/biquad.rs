use std::{
    ffi::c_void,
    num::NonZeroUsize,
    ops::{DerefMut, Range},
    ptr::{self, NonNull},
};

use super::ffi::{
    vDSP_biquadm, vDSP_biquadm_CopyState, vDSP_biquadm_CreateSetup, vDSP_biquadm_DestroySetup,
    vDSP_biquadm_ResetState, vDSP_biquadm_SetCoefficientsDouble,
};

mod consts {
    /// Coefficients per section: `b0, b1, b2, a1, a2`.
    pub(super) const SECTION: usize = 5;
    /// A section that passes its input through.
    pub(super) const IDENTITY: [f64; SECTION] = [1.0, 0.0, 0.0, 0.0, 0.0];
}

/// Why a multichannel biquad could not be built or run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum BiquadError {
    #[error("vDSP could not create the biquad setup")]
    Setup,
    #[error("buffer shape does not match the biquad setup")]
    Shape,
}

/// `vDSP_biquadm`: `sections` cascaded sections run in place on each of
/// `channels` planes. Every section starts as identity; `set_section` gives
/// one section the same coefficients on every channel:
/// `H(z) = (b0 + b1 z⁻¹ + b2 z⁻²) / (1 + a1 z⁻¹ + a2 z⁻²)`.
pub struct MultichannelBiquad {
    setup: NonNull<c_void>,
    channels: NonZeroUsize,
    sections: NonZeroUsize,
    planes: Box<[*mut f32]>,
    /// The coefficients the setup was created from and is retuned with:
    /// section `s` on every channel starts at `s × channels × 5`.
    table: Box<[f64]>,
}

// SAFETY: MultichannelBiquad owns its setup; every method takes &mut self or &self without mutation.
// SAFETY: The plane pointer table is refilled before each call and never read afterwards.
unsafe impl Send for MultichannelBiquad {}

impl MultichannelBiquad {
    /// # Errors
    /// [`BiquadError::Shape`] when `channels × sections × 5` overflows,
    /// [`BiquadError::Setup`] when vDSP returns no setup.
    pub fn new(channels: NonZeroUsize, sections: NonZeroUsize) -> Result<Self, BiquadError> {
        let table_len = channels
            .get()
            .checked_mul(sections.get())
            .and_then(|count| count.checked_mul(consts::SECTION))
            .ok_or(BiquadError::Shape)?;
        let table: Box<[f64]> = consts::IDENTITY
            .iter()
            .copied()
            .cycle()
            .take(table_len)
            .collect();
        // SAFETY: table holds sections × channels sections of five coefficients.
        let setup =
            unsafe { vDSP_biquadm_CreateSetup(table.as_ptr(), sections.get(), channels.get()) };
        let setup = NonNull::new(setup).ok_or(BiquadError::Setup)?;
        Ok(Self {
            setup,
            channels,
            sections,
            planes: vec![ptr::null_mut(); channels.get()].into_boxed_slice(),
            table,
        })
    }

    /// Gives `section` the same coefficients on every channel; the state stays.
    ///
    /// # Errors
    /// [`BiquadError::Shape`] when `section` is out of range.
    pub fn set_section(
        &mut self,
        section: usize,
        coefficients: [f32; consts::SECTION],
    ) -> Result<(), BiquadError> {
        let span = self.channels.get().saturating_mul(consts::SECTION);
        let block = self
            .table
            .chunks_exact_mut(span)
            .nth(section)
            .ok_or(BiquadError::Shape)?;
        for slot in block.as_chunks_mut::<{ consts::SECTION }>().0 {
            for (target, value) in slot.iter_mut().zip(coefficients) {
                *target = f64::from(value);
            }
        }
        // SAFETY: setup is live; block holds one section of five coefficients per channel.
        // SAFETY: section < sections, and the channel span 0..channels matches the setup.
        unsafe {
            vDSP_biquadm_SetCoefficientsDouble(
                self.setup.as_ptr(),
                block.as_ptr(),
                section,
                0,
                1,
                self.channels.get(),
            );
        }
        Ok(())
    }

    /// Filters `range` of every plane in place; an empty range is a no-op.
    ///
    /// # Errors
    /// [`BiquadError::Shape`] when the plane count differs from the channel
    /// count or a plane does not cover `range`.
    pub fn process<P: DerefMut<Target = [f32]>>(
        &mut self,
        planes: &mut [P],
        range: Range<usize>,
    ) -> Result<(), BiquadError> {
        if planes.len() != self.channels.get()
            || planes
                .iter()
                .any(|plane| plane.get(range.clone()).is_none())
        {
            return Err(BiquadError::Shape);
        }
        // A fixed native call size preserves stream prefixes across caller chunks.
        for frame in range {
            for (slot, plane) in self.planes.iter_mut().zip(planes.iter_mut()) {
                *slot = plane[frame..].as_mut_ptr();
            }
            let table = self.planes.as_mut_ptr();
            // SAFETY: setup is live and was created for self.channels channels.
            // SAFETY: every validated plane has one writable sample at frame.
            // SAFETY: X and Y name the same table, the in-place form of vDSP_biquadm.
            unsafe {
                vDSP_biquadm(
                    self.setup.as_ptr(),
                    table.cast::<*const f32>(),
                    1,
                    table,
                    1,
                    1,
                );
            }
        }
        Ok(())
    }

    /// Zeroes the state of every section and channel.
    pub fn reset(&mut self) {
        // SAFETY: setup is live.
        unsafe { vDSP_biquadm_ResetState(self.setup.as_ptr()) };
    }

    /// Takes over the state of `source`.
    ///
    /// # Errors
    /// [`BiquadError::Shape`] when channel or section counts differ.
    pub fn copy_state_from(&mut self, source: &Self) -> Result<(), BiquadError> {
        if self.channels != source.channels || self.sections != source.sections {
            return Err(BiquadError::Shape);
        }
        // SAFETY: both setups are live and share channel and section counts.
        unsafe { vDSP_biquadm_CopyState(self.setup.as_ptr(), source.setup.as_ptr()) };
        Ok(())
    }
}

impl Drop for MultichannelBiquad {
    fn drop(&mut self) {
        // SAFETY: setup came from vDSP_biquadm_CreateSetup and is destroyed exactly once.
        unsafe { vDSP_biquadm_DestroySetup(self.setup.as_ptr()) };
    }
}
