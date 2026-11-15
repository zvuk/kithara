use kithara::ui::render::{Node, ReadValue, Scope, StereoLevels};

use super::value::{Value, impl_child_node};
use crate::{
    engine::{DeckSnapshot, EngineSnapshot},
    mix::MixState,
};

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

/// The master bus. Its level comes from `MixState::levels` over the published
/// strips; an invalid mix answers nothing.
#[derive(Clone, Copy)]
pub(super) struct OutputNode<'a> {
    mix: &'a MixState,
    decks: &'a [DeckSnapshot],
}

impl<'a> OutputNode<'a> {
    pub(super) const fn new(mix: &'a MixState, decks: &'a [DeckSnapshot]) -> Self {
        Self { mix, decks }
    }

    fn passing(self) -> Option<f32> {
        let strips = self.decks.iter().map(|deck| &deck.strip);

        Some(
            self.mix
                .levels(strips)
                .ok()?
                .into_iter()
                .fold(0.0, f32::max),
        )
    }
}

impl_child_node!(OutputNode<'a>, |this, segment, _scope| {
    let value = match segment {
        "levels" => {
            let passing = this.passing()?;
            ReadValue::Stereo(StereoLevels {
                l: passing,
                r: passing,
                volume: this.mix.group_master,
            })
        }
        "volume" => ReadValue::Scalar(f64::from(this.mix.group_master)),
        _ => return None,
    };
    Some(Box::new(Value(value)))
});

#[derive(Clone, Copy)]
pub(super) struct PlayerNode<'a> {
    output: OutputNode<'a>,
}

impl<'a> PlayerNode<'a> {
    pub(super) const fn new(mix: &'a MixState, decks: &'a [DeckSnapshot]) -> Self {
        Self {
            output: OutputNode::new(mix, decks),
        }
    }
}

impl<'a> Node<'a> for PlayerNode<'a> {
    fn child(&self, segment: &str, _scope: Scope<'_>) -> Option<Box<dyn Node<'a> + 'a>> {
        match segment {
            "output" => Some(Box::new(self.output)),
            _ => None,
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct StripsNode<'a> {
    snapshot: &'a EngineSnapshot,
}

impl<'a> StripsNode<'a> {
    pub(super) const fn new(snapshot: &'a EngineSnapshot) -> Self {
        Self { snapshot }
    }
}

impl_child_node!(StripsNode<'a>, |this, segment, scope| {
    let strip = &this.snapshot.lettered(scope.get("deck")?)?.strip;

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
});
