use std::marker::PhantomData;

use kithara_command::{ScopeId, Seq};
use kithara_effects::{eq::StereoEq, filter::FilterEffect};
use kithara_signal::SessionEpoch;

/// Installed effect identity on a deck's output stream, typed by its effect.
///
/// ```compile_fail
/// use kithara_render::bridge::{EqBinding, FilterBinding};
/// fn wrong_effect(binding: FilterBinding) -> EqBinding { binding }
/// ```
#[derive_where::derive_where(Clone, Copy, Debug, Eq, PartialEq)]
#[derive(fieldwork::Fieldwork)]
#[fieldwork(get)]
pub struct EffectBinding<T> {
    #[field(get, copy)]
    scope: ScopeId,
    #[field(get, copy)]
    instance: Option<Seq>,
    #[field(get, copy)]
    epoch: SessionEpoch,
    #[field(skip)]
    effect: PhantomData<fn() -> T>,
}

impl<T> EffectBinding<T> {
    pub(crate) const fn new(scope: ScopeId, instance: Option<Seq>, epoch: SessionEpoch) -> Self {
        Self {
            scope,
            instance,
            epoch,
            effect: PhantomData,
        }
    }
}

/// The EQ layout and session axis on which a program was prepared.
pub type EqBinding = EffectBinding<StereoEq>;

/// The deck, installed Filter instance and output stream observed by a control.
pub type FilterBinding = EffectBinding<FilterEffect>;
