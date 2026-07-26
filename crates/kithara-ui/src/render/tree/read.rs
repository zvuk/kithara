use num_traits::cast::AsPrimitive;

use crate::{
    compile::CompiledUi,
    expand::Binding,
    render::{ReadValue, Reads},
    widgets::wave::zoom_math::DEFAULT_ZOOM,
};

pub(super) fn resolve<'a>(
    reads: &'a dyn Reads,
    binding: &Binding,
    ui: &CompiledUi,
) -> Option<ReadValue<'a>> {
    match binding {
        Binding::Command { .. } => None,
        binding => reads.get(ui.resolve(binding.key())),
    }
}

/// Scope suffix (`@deck=a` or empty) a widget appends to its derived endpoints.
pub(super) fn read_scope<'a>(read: Option<&Binding>, ui: &'a CompiledUi) -> &'a str {
    read.map_or("", |binding| {
        let key = ui.resolve(binding.key());
        let id_len = ui.resolve(binding.id()).len();
        key.get(id_len..).unwrap_or("")
    })
}

pub(super) fn read_flag(binding: Option<&Binding>, reads: &dyn Reads, ui: &CompiledUi) -> bool {
    matches!(
        binding.and_then(|binding| resolve(reads, binding, ui)),
        Some(ReadValue::Bool(true))
    )
}

pub(super) fn wave_zoom(zoom: Option<&Binding>, reads: &dyn Reads, ui: &CompiledUi) -> f32 {
    zoom.and_then(|binding| resolve(reads, binding, ui))
        .and_then(|value| match value {
            ReadValue::Scalar(value) => Some(value.as_()),
            _ => None,
        })
        .unwrap_or(DEFAULT_ZOOM)
}
