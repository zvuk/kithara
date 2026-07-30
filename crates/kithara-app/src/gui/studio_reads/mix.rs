use kithara_ui::render::{Node, ReadValue, Scope, StereoLevels};

use super::value::Value;
use crate::{gui::studio_ui::scope::deck_index, mix::MixState};

#[derive(Clone, Copy)]
pub(super) struct MixNode<'a> {
    mix: &'a MixState,
}

impl<'a> MixNode<'a> {
    pub(super) const fn new(mix: &'a MixState) -> Self {
        Self { mix }
    }
}

impl<'a> Node<'a> for MixNode<'a> {
    fn child(&self, segment: &str, _scope: Scope<'_>) -> Option<Box<dyn Node<'a> + 'a>> {
        let value = match segment {
            "crossfader" => ReadValue::Scalar(f64::from(self.mix.position)),
            _ => return None,
        };
        Some(Box::new(Value(value)))
    }
}

#[derive(Clone, Copy)]
pub(super) struct StripsNode<'a> {
    mix: &'a MixState,
}

impl<'a> StripsNode<'a> {
    pub(super) const fn new(mix: &'a MixState) -> Self {
        Self { mix }
    }
}

impl<'a> Node<'a> for StripsNode<'a> {
    fn child(&self, segment: &str, scope: Scope<'_>) -> Option<Box<dyn Node<'a> + 'a>> {
        let strip = self.mix.strips.get(deck_index(scope.get("deck")?)?)?;
        let value = match segment {
            "trim" => ReadValue::Scalar(f64::from(strip.trim)),
            "volume" => ReadValue::Stereo(StereoLevels {
                volume: strip.trim,
                ..StereoLevels::default()
            }),
            _ => return None,
        };
        Some(Box::new(Value(value)))
    }
}
