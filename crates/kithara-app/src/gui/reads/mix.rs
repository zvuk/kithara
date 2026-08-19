use kithara_ui::render::{Node, ReadValue, Scope, StereoLevels};

use super::value::Value;
use crate::{gui::ui::scope::deck_index, mix::MixState};

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

/// The master bus: what the mix hands the output, and the gain it is handed
/// through. `MixState::levels` is the only place that folds trim, mute, the
/// crossfader and the master together, so the meter reads it rather than
/// re-deriving a second answer. A rejected fold means the stored position is
/// not a mix at all, and the node answers nothing rather than a plausible zero.
#[derive(Clone, Copy)]
pub(super) struct OutputNode<'a> {
    mix: &'a MixState,
}

impl<'a> OutputNode<'a> {
    pub(super) const fn new(mix: &'a MixState) -> Self {
        Self { mix }
    }

    fn passing(self) -> Option<f32> {
        Some(self.mix.levels().ok()?.into_iter().fold(0.0, f32::max))
    }
}

impl<'a> Node<'a> for OutputNode<'a> {
    fn child(&self, segment: &str, _scope: Scope<'_>) -> Option<Box<dyn Node<'a> + 'a>> {
        let value = match segment {
            "levels" => {
                let passing = self.passing()?;
                ReadValue::Stereo(StereoLevels {
                    l: passing,
                    r: passing,
                    volume: self.mix.group_master,
                })
            }
            "volume" => ReadValue::Scalar(f64::from(self.mix.group_master)),
            _ => return None,
        };
        Some(Box::new(Value(value)))
    }
}

#[derive(Clone, Copy)]
pub(super) struct PlayerNode<'a> {
    mix: &'a MixState,
}

impl<'a> PlayerNode<'a> {
    pub(super) const fn new(mix: &'a MixState) -> Self {
        Self { mix }
    }
}

impl<'a> Node<'a> for PlayerNode<'a> {
    fn child(&self, segment: &str, _scope: Scope<'_>) -> Option<Box<dyn Node<'a> + 'a>> {
        match segment {
            "output" => Some(Box::new(OutputNode::new(self.mix))),
            _ => None,
        }
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
            "muted" => ReadValue::Bool(strip.muted),
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
