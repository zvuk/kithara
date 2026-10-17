use std::num::{NonZeroU32, NonZeroUsize};
/// Why a stream's geometry cannot size a deck's decoder buffers.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum BufferGeometryError {
    #[error("deck buffer geometry overflowed")]
    Overflow,
    #[error(
        "deck needs {required_frames} response frames for block {max_block_frames} and quantum {render_quantum_frames}, exceeding budget {budget_frames}"
    )]
    BudgetExceeded {
        max_block_frames: u32,
        render_quantum_frames: usize,
        required_frames: usize,
        budget_frames: usize,
    },
}

/// Stream dimensions needed to pre-size RT scratch buffers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StreamShape {
    pub max_block_frames: NonZeroU32,
    pub sample_rate: NonZeroU32,
}

impl StreamShape {
    #[must_use]
    pub const fn new(max_block_frames: NonZeroU32, sample_rate: NonZeroU32) -> Self {
        Self {
            max_block_frames,
            sample_rate,
        }
    }

    /// Compute decoder buffer depths, enforcing an application deadline when supplied.
    ///
    /// # Errors
    /// Returns an error when the geometry overflows or exceeds the budget.
    pub fn playback_buffers(
        self,
        quantum: NonZeroUsize,
        budget: Option<NonZeroUsize>,
    ) -> Result<(NonZeroUsize, NonZeroUsize), BufferGeometryError> {
        let output_frames = usize::try_from(self.max_block_frames.get())
            .map_err(|_| BufferGeometryError::Overflow)?;
        let preload = output_frames.div_ceil(quantum.get());
        let ring = preload
            .checked_add(1)
            .ok_or(BufferGeometryError::Overflow)?;
        let required_frames = ring
            .checked_add(1)
            .and_then(|chunks| chunks.checked_mul(quantum.get()))
            .and_then(|frames| frames.checked_sub(1))
            .ok_or(BufferGeometryError::Overflow)?;
        if let Some(budget) = budget
            && required_frames > budget.get()
        {
            return Err(BufferGeometryError::BudgetExceeded {
                required_frames,
                max_block_frames: self.max_block_frames.get(),
                render_quantum_frames: quantum.get(),
                budget_frames: budget.get(),
            });
        }
        Ok((
            NonZeroUsize::new(preload).ok_or(BufferGeometryError::Overflow)?,
            NonZeroUsize::new(ring).ok_or(BufferGeometryError::Overflow)?,
        ))
    }
}
