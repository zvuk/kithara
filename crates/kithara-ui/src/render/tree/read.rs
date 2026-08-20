use num_traits::cast::AsPrimitive;

use crate::{
    compile::CompiledUi,
    expand::{Binding, BindingKind, BlockSpec},
    render::{ReadValue, Reads},
    size::Snapshot,
    widgets::wave::zoom_math::DEFAULT_ZOOM,
};

/// What the host answers this frame, read through the compiled arena.
#[derive(Clone, Copy)]
pub(super) struct Answers<'a> {
    pub(super) reads: &'a dyn Reads,
    pub(super) ui: &'a CompiledUi,
}

impl Snapshot for Answers<'_> {
    fn hidden(&self, block: &BlockSpec) -> bool {
        read_flag(Some(&block.hidden), self.reads, self.ui)
    }

    fn measure(&self, measure: &Binding) -> Option<f32> {
        read_measure(measure, self.reads, self.ui)
    }
}

pub(super) fn resolve<'a>(
    reads: &'a dyn Reads,
    binding: &Binding,
    ui: &CompiledUi,
) -> Option<ReadValue<'a>> {
    match binding.kind {
        BindingKind::Command => None,
        _ => reads.get(ui.resolve(binding.key)),
    }
}

pub(super) fn read_scope<'a>(read: Option<&Binding>, ui: &'a CompiledUi) -> &'a str {
    read.map_or("", |binding| {
        let key = ui.resolve(binding.key);
        let id_len = ui.resolve(binding.id).len();
        key.get(id_len..).unwrap_or("")
    })
}

pub(super) fn read_flag(binding: Option<&Binding>, reads: &dyn Reads, ui: &CompiledUi) -> bool {
    matches!(
        binding.and_then(|binding| resolve(reads, binding, ui)),
        Some(ReadValue::Bool(true))
    )
}

/// The one place an adaptive measure crosses from the host's `f64` into the
/// `f32` the thresholds are written in; a value that survives neither the cast
/// nor the read is no measurement.
fn read_measure(binding: &Binding, reads: &dyn Reads, ui: &CompiledUi) -> Option<f32> {
    let Some(ReadValue::Scalar(value)) = resolve(reads, binding, ui) else {
        return None;
    };
    let value: f32 = value.as_();
    value.is_finite().then_some(value)
}

pub(super) fn wave_zoom(zoom: Option<&Binding>, reads: &dyn Reads, ui: &CompiledUi) -> f32 {
    zoom.and_then(|binding| resolve(reads, binding, ui))
        .and_then(|value| match value {
            ReadValue::Scalar(value) => Some(value.as_()),
            _ => None,
        })
        .unwrap_or(DEFAULT_ZOOM)
}
